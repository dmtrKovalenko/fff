use bytemuck::{Pod, Zeroable};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

const SLOTS: u32 = 1;
pub const MAX_NEEDLE: usize = 64;
const OUT_CAP: usize = 1024;
const TIE_CAP: usize = 16384;
const TIE_BUCKETS: usize = 4096;
const OUT_STRIDE: usize = 4;
const TIE_STRIDE: usize = 2;
const TOPK_STAGING: u64 =
    (16 + OUT_CAP * OUT_STRIDE * 4 + TIE_CAP * TIE_STRIDE * 4 + TIE_BUCKETS * 4 + 4) as u64;
const SOA_GROUP: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    needle_len: u32,
    item_count: u32,
    match_score: u32,
    mismatch_penalty: u32,
    gap_open: u32,
    gap_extend: u32,
    prefix_bonus: u32,
    delimiter_bonus: u32,
    capitalization_bonus: u32,
    matching_case_bonus: u32,
    exact_match_bonus: u32,
    thread_count: u32,
    _reserved: u32,
    top_k: u32,
    tie_bucket: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kernel {
    ThreadPerItem,
    LanePerThread,
    Subgroup,
    Scalar,
}

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
}

// Survivor list plus everything bound to it; two sets alternate so an
// incremental prefilter can read the previous list while writing the new one.
struct SurvivorSet {
    count: wgpu::Buffer,
    prefilter: Pass,
    prefilter_inc: Pass,
    args: Pass,
    lanes: Pass,
    hist: Pass,
    compact: Pass,
    compact2: Pass,
    dp_entries: Vec<wgpu::Buffer>,
    dp_entries_scalar: Vec<wgpu::Buffer>,
}

pub struct GpuMatcher {
    device: wgpu::Device,
    queue: wgpu::Queue,
    kernel: Kernel,
    item_pass: Option<Pass>,
    sets: Vec<SurvivorSet>,
    select_pass: Pass,
    cur: Cell<usize>,
    last_needle: RefCell<Vec<u8>>,
    incremental: Cell<bool>,
    subgroup_passes: RefCell<HashMap<(usize, usize), Pass>>,
    scalar_passes: RefCell<HashMap<(usize, usize), Pass>>,
    subgroup_unsupported: RefCell<std::collections::HashSet<usize>>,
    params_buf: wgpu::Buffer,
    needle_buf: wgpu::Buffer,
    indirect_buf: wgpu::Buffer,
    scores_buf: wgpu::Buffer,
    staging_buf: wgpu::Buffer,
    sel_buf: wgpu::Buffer,
    out_buf: wgpu::Buffer,
    ties_buf: wgpu::Buffer,
    tie_hist_buf: wgpu::Buffer,
    tie_counter_buf: wgpu::Buffer,
    topk_staging: wgpu::Buffer,
    ties_staging: wgpu::Buffer,
    hay_buf: wgpu::Buffer,
    items_buf: wgpu::Buffer,
    soa_buf: wgpu::Buffer,
    soa_ids_buf: wgpu::Buffer,
    soa_start_buf: wgpu::Buffer,
    fname_buf: wgpu::Buffer,
    boost_buf: wgpu::Buffer,
    item_count: Cell<u32>,
    thread_count: Cell<u32>,
    hay_words: Cell<usize>,
    soa_words: Cell<usize>,
    item_cap: usize,
    hay_cap_words: usize,
    soa_cap_words: usize,
    pub adapter_name: String,
    pub lanes: u32,
    timestamps: Option<Timestamps>,
    pub pass_times: RefCell<Vec<(f64, f64, f64)>>,
}

struct Timestamps {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    staging: wgpu::Buffer,
    period: f32,
}

enum Readback {
    Scores(Vec<u32>, Vec<u32>),
    TopK(TopK),
}

// Sorted (score desc, id asc) candidates plus how many paths matched at all.
// `meta[i]` is `end_col | exact << 16` for `entries[i]` (scalar kernel only).
#[derive(Debug, Clone, PartialEq)]
pub struct TopK {
    pub entries: Vec<(u32, u32)>,
    pub meta: Vec<u32>,
    pub matched: u32,
}

pub struct Reserve {
    pub items: usize,
    pub bytes: usize,
}

#[derive(Debug)]
pub struct ReserveExhausted;

impl GpuMatcher {
    pub fn new(paths: &[String], kernel: Kernel) -> Self {
        Self::with_reserve(paths, None, kernel, Reserve { items: 0, bytes: 0 })
    }

    // `fname_off[i]` is the byte offset of the filename inside path i (None:
    // no filename bonus). `reserve` leaves room for `append` without a rebuild.
    pub fn with_reserve(
        paths: &[String],
        fname_off: Option<&[u32]>,
        kernel: Kernel,
        reserve: Reserve,
    ) -> Self {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("no GPU adapter");
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", info.name, info.backend);
        let lanes = match std::env::var("LANES") {
            Ok(v) => v.parse().unwrap(),
            // wgpu buckets the reported range (4..=64 on Apple), so key on the backend.
            Err(_) if info.backend == wgpu::Backend::Metal => 32,
            Err(_) => 16,
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: adapter.features()
                & (wgpu::Features::SUBGROUP | wgpu::Features::TIMESTAMP_QUERY),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .expect("device");

        let (hay, items) = pack_paths(paths, 0);
        let (soa, soa_start, soa_ids) = pack_paths_soa(paths, 0, true);
        let item_count = paths.len() as u32;
        let item_cap = paths.len() + reserve.items;
        let hay_cap_words = hay.len() + reserve.bytes.div_ceil(4) + reserve.items;
        let soa_cap_words = soa.len() + reserve.bytes.div_ceil(4) * 2 + reserve.items * SOA_GROUP;
        let thread_cap = item_cap.div_ceil(SOA_GROUP) * SOA_GROUP + SOA_GROUP;
        let n1 = item_cap.max(1) as u64;
        let fname: Vec<u32> = match fname_off {
            Some(f) => f.to_vec(),
            None => vec![u32::MAX; paths.len()],
        };

        let storage = |label: &str, size: u64, extra: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE | extra,
                mapped_at_creation: false,
            })
        };
        let staging = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let needle_buf = storage("needle", 64, wgpu::BufferUsages::COPY_DST);
        let init_cap = |label: &str, contents: &[u8], cap_bytes: u64| {
            let b = storage(label, cap_bytes.max(4), wgpu::BufferUsages::COPY_DST);
            if !contents.is_empty() {
                queue.write_buffer(&b, 0, contents);
            }
            b
        };
        let hay_buf = init_cap(
            "hay",
            bytemuck::cast_slice(&hay),
            (hay_cap_words as u64 + 1) * 4,
        );
        let items_buf = init_cap("items", bytemuck::cast_slice(&items), n1 * 8);
        let soa_buf = init_cap(
            "hay_soa",
            bytemuck::cast_slice(&soa),
            soa_cap_words as u64 * 4,
        );
        let soa_ids_buf = init_cap(
            "soa_ids",
            bytemuck::cast_slice(&soa_ids),
            thread_cap as u64 * 4,
        );
        let soa_start_buf = init_cap("soa_start", bytemuck::cast_slice(&soa_start), n1 * 4);
        let fname_buf = init_cap("fname", bytemuck::cast_slice(&fname), n1 * 4);
        let boost_buf = storage("boost", n1 * 4, wgpu::BufferUsages::COPY_DST);
        let meta_buf = storage("meta", n1 * 4, wgpu::BufferUsages::COPY_SRC);
        let scores_buf = storage("scores", n1 * 4, wgpu::BufferUsages::COPY_SRC);
        let staging_buf = staging("staging", n1 * 8);
        let indirect_buf = storage("indirect", 36, wgpu::BufferUsages::INDIRECT);
        let hist_buf = storage("hist", 4096 * 4, wgpu::BufferUsages::COPY_DST);
        let sel_buf = storage("sel", 16, wgpu::BufferUsages::COPY_SRC);
        let out_buf = storage(
            "out",
            (OUT_CAP * OUT_STRIDE * 4) as u64,
            wgpu::BufferUsages::COPY_SRC,
        );
        let ties_buf = storage(
            "ties",
            (TIE_CAP * TIE_STRIDE * 4) as u64,
            wgpu::BufferUsages::COPY_SRC,
        );
        let topk_staging = staging("topk-staging", TOPK_STAGING);
        let tie_hist_buf = storage(
            "tie_hist",
            (TIE_BUCKETS * 4) as u64,
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        );
        let tie_counter_buf = storage(
            "tie_counter",
            4,
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        );
        let ties_staging = staging("ties-staging", (4 + TIE_CAP * TIE_STRIDE * 4) as u64);

        let mk = |label: &str, source: &str, bufs: &[&wgpu::Buffer]| {
            make_pass(&device, label, source, bufs)
        };
        let item_pass = (kernel == Kernel::ThreadPerItem).then(|| {
            mk(
                "score",
                include_str!("score.wgsl"),
                &[&params_buf, &needle_buf, &hay_buf, &items_buf, &scores_buf],
            )
        });
        let select_pass = mk(
            "select",
            include_str!("select.wgsl"),
            &[&params_buf, &hist_buf, &sel_buf],
        );

        let survivor_bufs: Vec<(wgpu::Buffer, wgpu::Buffer)> = (0..2)
            .map(|i| {
                (
                    storage(
                        &format!("survivors{i}"),
                        n1 * 16,
                        wgpu::BufferUsages::empty(),
                    ),
                    storage(
                        &format!("count{i}"),
                        4,
                        wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                    ),
                )
            })
            .collect();
        let sets: Vec<SurvivorSet> = (0..2)
            .map(|i| {
                let (survivors, count) = &survivor_bufs[i];
                let (prev_survivors, prev_count) = &survivor_bufs[i ^ 1];
                let dp_entries = vec![
                    params_buf.clone(),
                    needle_buf.clone(),
                    hay_buf.clone(),
                    items_buf.clone(),
                    scores_buf.clone(),
                    survivors.clone(),
                    count.clone(),
                ];
                let dp_refs: Vec<&wgpu::Buffer> = dp_entries.iter().collect();
                let mut dp_entries_scalar = dp_entries.clone();
                dp_entries_scalar.extend([meta_buf.clone(), fname_buf.clone(), boost_buf.clone()]);
                SurvivorSet {
                    prefilter: mk(
                        "prefilter",
                        include_str!("prefilter.wgsl"),
                        &[
                            &params_buf,
                            &needle_buf,
                            &items_buf,
                            &scores_buf,
                            survivors,
                            count,
                            &soa_start_buf,
                            &soa_buf,
                            &soa_ids_buf,
                        ],
                    ),
                    prefilter_inc: mk(
                        "prefilter_inc",
                        include_str!("prefilter_inc.wgsl"),
                        &[
                            &params_buf,
                            &needle_buf,
                            &items_buf,
                            &scores_buf,
                            survivors,
                            count,
                            &soa_start_buf,
                            &soa_buf,
                            prev_survivors,
                            prev_count,
                        ],
                    ),
                    args: mk(
                        "args",
                        include_str!("args.wgsl"),
                        &[count, &indirect_buf, &hist_buf, &tie_hist_buf, prev_count],
                    ),
                    lanes: mk("lanes", include_str!("score_lanes.wgsl"), &dp_refs),
                    hist: mk(
                        "hist",
                        include_str!("hist.wgsl"),
                        &[&scores_buf, survivors, count, &hist_buf],
                    ),
                    compact: mk(
                        "compact",
                        include_str!("compact.wgsl"),
                        &[
                            &scores_buf,
                            survivors,
                            count,
                            &sel_buf,
                            &out_buf,
                            &ties_buf,
                            &tie_hist_buf,
                            &meta_buf,
                        ],
                    ),
                    compact2: mk(
                        "compact2",
                        include_str!("compact2.wgsl"),
                        &[
                            &params_buf,
                            &scores_buf,
                            survivors,
                            count,
                            &sel_buf,
                            &tie_counter_buf,
                            &ties_buf,
                            &meta_buf,
                        ],
                    ),
                    count: count.clone(),
                    dp_entries,
                    dp_entries_scalar,
                }
            })
            .collect();

        let timestamps = (device.features().contains(wgpu::Features::TIMESTAMP_QUERY)
            && std::env::var("GPU_TIMING").is_ok())
        .then(|| Timestamps {
            set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("ts"),
                ty: wgpu::QueryType::Timestamp,
                count: 6,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ts-resolve"),
                size: 48,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            staging: staging("ts-staging", 48),
            period: queue.get_timestamp_period(),
        });

        Self {
            device,
            queue,
            kernel,
            item_pass,
            sets,
            select_pass,
            cur: Cell::new(0),
            last_needle: RefCell::new(Vec::new()),
            incremental: Cell::new(std::env::var("NO_INCREMENTAL").is_err()),
            subgroup_passes: RefCell::new(HashMap::new()),
            scalar_passes: RefCell::new(HashMap::new()),
            subgroup_unsupported: RefCell::new(Default::default()),
            params_buf,
            needle_buf,
            indirect_buf,
            scores_buf,
            staging_buf,
            sel_buf,
            out_buf,
            ties_buf,
            tie_hist_buf,
            tie_counter_buf,
            topk_staging,
            ties_staging,
            hay_buf,
            items_buf,
            soa_buf,
            soa_ids_buf,
            soa_start_buf,
            fname_buf,
            boost_buf,
            item_count: Cell::new(item_count),
            thread_count: Cell::new(soa_ids.len() as u32),
            hay_words: Cell::new(hay.len()),
            soa_words: Cell::new(soa.len()),
            item_cap,
            hay_cap_words,
            soa_cap_words,
            adapter_name,
            lanes,
            timestamps,
            pass_times: RefCell::new(Vec::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.item_count.get() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Adds paths (ids continue from `len()`). Err when the reserve is used up.
    pub fn append(
        &self,
        paths: &[String],
        fname_off: Option<&[u32]>,
    ) -> Result<(), ReserveExhausted> {
        if paths.is_empty() {
            return Ok(());
        }
        let id0 = self.len();
        let (hay, items) = pack_paths(paths, self.hay_words.get());
        let (soa, soa_start, soa_ids) = pack_paths_soa(paths, id0, false);
        let hay_words = self.hay_words.get() + hay.len();
        let soa_words = self.soa_words.get() + soa.len();
        let threads = self.thread_count.get() as usize + soa_ids.len();
        if id0 + paths.len() > self.item_cap
            || hay_words > self.hay_cap_words
            || soa_words > self.soa_cap_words
            || threads > self.item_cap.div_ceil(SOA_GROUP) * SOA_GROUP + SOA_GROUP
        {
            return Err(ReserveExhausted);
        }
        let soa_start: Vec<u32> = soa_start
            .into_iter()
            .map(|s| s + self.soa_words.get() as u32)
            .collect();
        let fname: Vec<u32> = match fname_off {
            Some(f) => f.to_vec(),
            None => vec![u32::MAX; paths.len()],
        };
        let w = |b: &wgpu::Buffer, off: usize, data: &[u8]| {
            self.queue.write_buffer(b, off as u64 * 4, data);
        };
        w(
            &self.hay_buf,
            self.hay_words.get(),
            bytemuck::cast_slice(&hay),
        );
        w(&self.items_buf, id0 * 2, bytemuck::cast_slice(&items));
        w(
            &self.soa_buf,
            self.soa_words.get(),
            bytemuck::cast_slice(&soa),
        );
        w(
            &self.soa_ids_buf,
            self.thread_count.get() as usize,
            bytemuck::cast_slice(&soa_ids),
        );
        w(&self.soa_start_buf, id0, bytemuck::cast_slice(&soa_start));
        w(&self.fname_buf, id0, bytemuck::cast_slice(&fname));
        self.hay_words.set(hay_words);
        self.soa_words.set(soa_words);
        self.thread_count.set(threads as u32);
        self.item_count.set((id0 + paths.len()) as u32);
        // New items are not in any previous survivor list.
        self.last_needle.borrow_mut().clear();
        Ok(())
    }

    // Per-item percent boost added to the fuzzy score in the scalar kernel.
    pub fn set_boosts(&self, boosts: &[i32]) {
        let n = boosts.len().min(self.len());
        if n > 0 {
            self.queue
                .write_buffer(&self.boost_buf, 0, bytemuck::cast_slice(&boosts[..n]));
        }
    }

    pub fn set_incremental(&self, on: bool) {
        self.incremental.set(on);
        self.last_needle.borrow_mut().clear();
    }

    // Trivial one-thread dispatch to keep the GPU clocked up between queries.
    pub fn nudge(&self) {
        let args = &self.sets[self.cur.get()].args;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&args.pipeline);
            pass.set_bind_group(0, &args.bind_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        self.queue.submit([encoder.finish()]);
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
    }

    // Median (prefilter, dp, topk) GPU time in ms since the last call.
    pub fn take_pass_medians(&self) -> Option<(f64, f64, f64)> {
        let v = std::mem::take(&mut *self.pass_times.borrow_mut());
        if v.is_empty() {
            return None;
        }
        let med = |f: fn(&(f64, f64, f64)) -> f64| {
            let mut a: Vec<f64> = v.iter().map(f).collect();
            a.sort_by(f64::total_cmp);
            a[a.len() / 2]
        };
        Some((med(|x| x.0), med(|x| x.1), med(|x| x.2)))
    }

    pub fn uses_scalar(&self, n: usize) -> bool {
        match self.kernel {
            Kernel::Scalar => true,
            Kernel::Subgroup => {
                let max = std::env::var("SCALAR_MAX")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0usize);
                n <= max
            }
            _ => false,
        }
    }

    // One score per item. The subgroup kernel falls back to the barrier
    // kernel when the driver picks a narrower subgroup for this needle length.
    pub fn search(&self, needle: &str, scoring: &neo_frizbee::Scoring) -> Vec<u32> {
        self.search_full(needle, scoring).0
    }

    // Scores plus per-item meta (`end_col | exact << 16`, scalar kernel only).
    pub fn search_full(
        &self,
        needle: &str,
        scoring: &neo_frizbee::Scoring,
    ) -> (Vec<u32>, Vec<u32>) {
        let n = needle.len().min(MAX_NEEDLE);
        let use_subgroup = !self.uses_scalar(n)
            && self.kernel == Kernel::Subgroup
            && !self.subgroup_unsupported.borrow().contains(&n);
        let Readback::Scores(scores, meta) = self.search_with(needle, scoring, use_subgroup, None)
        else {
            unreachable!()
        };
        if use_subgroup && scores.contains(&u32::MAX) {
            eprintln!("subgroup kernel unsupported for needle len {n}, falling back");
            self.subgroup_unsupported.borrow_mut().insert(n);
            let Readback::Scores(scores, meta) = self.search_with(needle, scoring, false, None)
            else {
                unreachable!()
            };
            return (scores, meta);
        }
        (scores, meta)
    }

    // Sorted (score desc, id asc) top-k selected on the GPU; only the
    // candidates come back. Falls back to a full readback on huge tie groups.
    pub fn search_topk(&self, needle: &str, scoring: &neo_frizbee::Scoring, k: usize) -> TopK {
        assert!(k <= OUT_CAP && self.kernel != Kernel::ThreadPerItem);
        let n = needle.len().min(MAX_NEEDLE);
        let use_subgroup = !self.uses_scalar(n)
            && self.kernel == Kernel::Subgroup
            && !self.subgroup_unsupported.borrow().contains(&n);
        match self.search_with(needle, scoring, use_subgroup, Some(k as u32)) {
            Readback::TopK(v) => v,
            Readback::Scores(scores, meta) => {
                let mut top = crate::top_k_scores(&scores, k);
                top.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
                let matched = scores.iter().filter(|&&s| s > 0).count() as u32;
                TopK {
                    meta: top.iter().map(|e| meta[e.1 as usize]).collect(),
                    entries: top,
                    matched,
                }
            }
        }
    }

    fn search_with(
        &self,
        needle: &str,
        scoring: &neo_frizbee::Scoring,
        use_subgroup: bool,
        topk: Option<u32>,
    ) -> Readback {
        let t0 = std::time::Instant::now();
        let needle_bytes = &needle.as_bytes()[..needle.len().min(MAX_NEEDLE)];
        let n = needle_bytes.len();
        let mut packed = [0u8; 64];
        packed[..n].copy_from_slice(needle_bytes);
        let scalar = self.uses_scalar(n);

        let item_count = self.item_count.get();
        let params = Params {
            needle_len: n as u32,
            item_count,
            match_score: scoring.match_score as u32,
            mismatch_penalty: scoring.mismatch_penalty as u32,
            gap_open: scoring.gap_open_penalty as u32,
            gap_extend: scoring.gap_extend_penalty as u32,
            prefix_bonus: scoring.prefix_bonus as u32,
            delimiter_bonus: scoring.delimiter_bonus as u32,
            capitalization_bonus: scoring.capitalization_bonus as u32,
            matching_case_bonus: scoring.matching_case_bonus as u32,
            exact_match_bonus: scoring.exact_match_bonus as u32,
            thread_count: self.thread_count.get(),
            _reserved: 0,
            top_k: topk.unwrap_or(0),
            tie_bucket: 0,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));
        self.queue.write_buffer(&self.needle_buf, 0, &packed);

        let out = self.cur.get() ^ 1;
        let set = &self.sets[out];
        let incremental = {
            let last = self.last_needle.borrow();
            self.incremental.get()
                && self.kernel != Kernel::ThreadPerItem
                && !last.is_empty()
                && n >= last.len()
                && needle_bytes.starts_with(&last)
        };

        let mut encoder = self.device.create_command_encoder(&Default::default());
        let ts_writes = |a: u32, b: u32| {
            self.timestamps
                .as_ref()
                .map(|t| wgpu::ComputePassTimestampWrites {
                    query_set: &t.set,
                    beginning_of_pass_write_index: Some(a),
                    end_of_pass_write_index: Some(b),
                })
        };
        macro_rules! compute {
            ($a:expr, $b:expr) => {
                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: None,
                    timestamp_writes: ts_writes($a, $b),
                })
            };
        }
        let dispatch = |pass: &mut wgpu::ComputePass, p: &Pass, x: u32, y: u32| {
            pass.set_pipeline(&p.pipeline);
            pass.set_bind_group(0, &p.bind_group, &[]);
            pass.dispatch_workgroups(x, y, 1);
        };
        let dispatch_indirect = |pass: &mut wgpu::ComputePass, p: &Pass, offset: u64| {
            pass.set_pipeline(&p.pipeline);
            pass.set_bind_group(0, &p.bind_group, &[]);
            pass.dispatch_workgroups_indirect(&self.indirect_buf, offset);
        };

        if let Some(item_pass) = &self.item_pass {
            let mut pass = compute!(0, 1);
            let (x, y) = grid(item_count.div_ceil(64));
            dispatch(&mut pass, item_pass, x, y);
        } else {
            let timing = self.timestamps.is_some();
            {
                let mut pass = compute!(0, 1);
                if incremental {
                    // Slot 2 still holds the previous survivor count in 64-groups.
                    dispatch_indirect(&mut pass, &set.prefilter_inc, 12);
                } else {
                    let (x, y) = grid(self.thread_count.get().div_ceil(64));
                    dispatch(&mut pass, &set.prefilter, x, y);
                }
                dispatch(&mut pass, &set.args, 1, 1);
                if timing {
                    drop(pass);
                    pass = compute!(2, 3);
                }
                let mut cache = self.subgroup_passes.borrow_mut();
                let mut scache = self.scalar_passes.borrow_mut();
                let refs: Vec<&wgpu::Buffer> = set.dp_entries.iter().collect();
                let refs_scalar: Vec<&wgpu::Buffer> = set.dp_entries_scalar.iter().collect();
                let score_pass = if scalar {
                    scache.entry((n, out)).or_insert_with(|| {
                        make_pass(
                            &self.device,
                            "scalar",
                            &crate::scalar::generate(n),
                            &refs_scalar,
                        )
                    })
                } else if use_subgroup {
                    cache.entry((n, out)).or_insert_with(|| {
                        make_pass(
                            &self.device,
                            "subgroup",
                            &crate::subgroup::generate(n, self.lanes),
                            &refs,
                        )
                    })
                } else {
                    &set.lanes
                };
                if std::env::var("GPU_SKIP_DP").is_err() {
                    dispatch_indirect(&mut pass, score_pass, if scalar { 12 } else { 0 });
                }
                if topk.is_some() {
                    if timing {
                        drop(pass);
                        pass = compute!(4, 5);
                    }
                    dispatch_indirect(&mut pass, &set.hist, 24);
                    dispatch(&mut pass, &self.select_pass, 1, 1);
                    dispatch_indirect(&mut pass, &set.compact, 12);
                }
            }
        }
        if let Some(t) = &self.timestamps {
            encoder.resolve_query_set(&t.set, 0..6, &t.resolve, 0);
            encoder.copy_buffer_to_buffer(&t.resolve, 0, &t.staging, 0, 48);
        }
        if topk.is_some() {
            encoder.copy_buffer_to_buffer(&self.sel_buf, 0, &self.topk_staging, 0, 16);
            let out_bytes = (OUT_CAP * OUT_STRIDE * 4) as u64;
            let tie_bytes = (TIE_CAP * TIE_STRIDE * 4) as u64;
            encoder.copy_buffer_to_buffer(&self.out_buf, 0, &self.topk_staging, 16, out_bytes);
            encoder.copy_buffer_to_buffer(
                &self.ties_buf,
                0,
                &self.topk_staging,
                16 + out_bytes,
                tie_bytes,
            );
            encoder.copy_buffer_to_buffer(
                &self.tie_hist_buf,
                0,
                &self.topk_staging,
                16 + out_bytes + tie_bytes,
                (TIE_BUCKETS * 4) as u64,
            );
            encoder.copy_buffer_to_buffer(
                &set.count,
                0,
                &self.topk_staging,
                16 + out_bytes + tie_bytes + (TIE_BUCKETS * 4) as u64,
                4,
            );
        } else {
            let bytes = item_count.max(1) as u64 * 4;
            encoder.copy_buffer_to_buffer(&self.scores_buf, 0, &self.staging_buf, 0, bytes);
            encoder.copy_buffer_to_buffer(&self.meta_buf(), 0, &self.staging_buf, bytes, bytes);
        }
        let count_staging = self.timestamps.as_ref().map(|_| {
            let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("count-staging"),
                size: 4,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_buffer_to_buffer(&set.count, 0, &b, 0, 4);
            b
        });
        self.queue.submit([encoder.finish()]);
        if self.item_pass.is_none() {
            self.cur.set(out);
            *self.last_needle.borrow_mut() = needle_bytes.to_vec();
        }
        let t1 = t0.elapsed();

        let result = if let Some(k) = topk {
            let raw = self.map_read(&self.topk_staging);
            let words: &[u32] = bytemuck::cast_slice(&raw);
            let (t, out_count, tie_count) = (words[0], words[2], words[3]);
            let out_words = OUT_CAP * OUT_STRIDE;
            let tie_words = TIE_CAP * TIE_STRIDE;
            let entries = &words[4..4 + out_words];
            let mut top: Vec<(u32, u32, u32)> = entries
                .chunks_exact(OUT_STRIDE)
                .take((out_count as usize).min(OUT_CAP))
                .map(|e| (e[0], e[1], e[2]))
                .collect();
            top.sort_by(|a, b| (b.0, a.1).cmp(&(a.0, b.1)));
            let need = (k as usize).saturating_sub(top.len());
            let mut ties: Vec<(u32, u32)> = if tie_count as usize <= TIE_CAP {
                words[4 + out_words..][..tie_count as usize * TIE_STRIDE]
                    .chunks_exact(TIE_STRIDE)
                    .map(|e| (e[0], e[1]))
                    .collect()
            } else {
                // Oversized tie group: find the id bucket holding the need-th
                // smallest id, then fetch only ties up to that bucket.
                let tie_hist = &words[4 + out_words + tie_words..][..TIE_BUCKETS];
                let mut acc = 0usize;
                let mut bucket = TIE_BUCKETS as u32 - 1;
                for (b, &c) in tie_hist.iter().enumerate() {
                    acc += c as usize;
                    if acc >= need {
                        bucket = b as u32;
                        break;
                    }
                }
                let mut params = params;
                params.tie_bucket = bucket;
                self.queue
                    .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));
                self.queue.write_buffer(&self.tie_counter_buf, 0, &[0u8; 4]);
                let mut enc = self.device.create_command_encoder(&Default::default());
                {
                    let mut pass = enc.begin_compute_pass(&Default::default());
                    dispatch_indirect(&mut pass, &set.compact2, 12);
                }
                enc.copy_buffer_to_buffer(&self.tie_counter_buf, 0, &self.ties_staging, 0, 4);
                enc.copy_buffer_to_buffer(
                    &self.ties_buf,
                    0,
                    &self.ties_staging,
                    4,
                    (TIE_CAP * TIE_STRIDE * 4) as u64,
                );
                self.queue.submit([enc.finish()]);
                let raw = self.map_read(&self.ties_staging);
                let w: &[u32] = bytemuck::cast_slice(&raw);
                w[1..][..(w[0] as usize).min(TIE_CAP) * TIE_STRIDE]
                    .chunks_exact(TIE_STRIDE)
                    .map(|e| (e[0], e[1]))
                    .collect()
            };
            ties.sort_unstable();
            top.extend(ties.into_iter().take(need).map(|(id, m)| (t, id, m)));
            Readback::TopK(TopK {
                entries: top.iter().map(|e| (e.0, e.1)).collect(),
                meta: top.iter().map(|e| e.2).collect(),
                matched: words[4 + out_words + tie_words + TIE_BUCKETS],
            })
        } else {
            let raw = self.map_read(&self.staging_buf);
            let words: &[u32] = bytemuck::cast_slice(&raw);
            let n = item_count as usize;
            Readback::Scores(words[..n].to_vec(), words[n.max(1)..][..n].to_vec())
        };
        let t2 = t0.elapsed();

        if let Some(t) = &self.timestamps {
            let raw = self.map_read(&t.staging);
            let ts: &[u64] = bytemuck::cast_slice(&raw);
            let ms = |a: u64, b: u64| (b.saturating_sub(a)) as f64 * t.period as f64 / 1e6;
            self.pass_times.borrow_mut().push((
                ms(ts[0], ts[1]),
                ms(ts[2], ts[3]),
                if topk.is_some() {
                    ms(ts[4], ts[5])
                } else {
                    0.0
                },
            ));
        }
        if let Some(b) = count_staging {
            let raw = self.map_read(&b);
            let survivors = u32::from_le_bytes(raw[..4].try_into().unwrap());
            eprintln!(
                "  {needle:<12} submit {:>4}µs  gpu+read {:>5}µs  survivors {survivors}{}",
                t1.as_micros(),
                (t2 - t1).as_micros(),
                if incremental { "  (incremental)" } else { "" }
            );
        }
        result
    }

    fn meta_buf(&self) -> wgpu::Buffer {
        self.sets[0].dp_entries_scalar[7].clone()
    }

    fn map_read(&self, buf: &wgpu::Buffer) -> Vec<u8> {
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        rx.recv().unwrap().expect("map");
        let out = slice.get_mapped_range().expect("range").to_vec();
        buf.unmap();
        out
    }
}

fn make_pass(device: &wgpu::Device, label: &str, source: &str, bufs: &[&wgpu::Buffer]) -> Pass {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(
            source
                .replace("SLOTS_PLACEHOLDER", &SLOTS.to_string())
                .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let entries: Vec<wgpu::BindGroupEntry> = bufs
        .iter()
        .enumerate()
        .map(|(i, b)| wgpu::BindGroupEntry {
            binding: i as u32,
            resource: b.as_entire_binding(),
        })
        .collect();
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &entries,
    });
    Pass {
        pipeline,
        bind_group,
    }
}

// 64-path groups interleaved by word, padded to the longest path in the group.
// With `sort`, groups come from length-sorted 4096-path blocks so padding
// stays small. `starts` is per path (id order), `ids[thread]` maps a
// prefilter thread back to its path (u32::MAX = padding), ids offset by `id0`.
fn pack_paths_soa(paths: &[String], id0: usize, sort: bool) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    const G: usize = SOA_GROUP;
    const BLOCK: usize = 4096;
    let mut soa = Vec::new();
    let mut starts = vec![0u32; paths.len()];
    let mut ids = Vec::with_capacity(paths.len().div_ceil(G) * G);
    for (b, block) in paths.chunks(BLOCK).enumerate() {
        let mut order: Vec<usize> = (0..block.len()).map(|i| b * BLOCK + i).collect();
        if sort {
            order.sort_by_key(|&i| paths[i].len());
        }
        for group in order.chunks(G) {
            let max_words = group
                .iter()
                .map(|&i| paths[i].len().div_ceil(4))
                .max()
                .unwrap_or(0);
            let base = soa.len();
            soa.resize(base + max_words * G, 0u32);
            for lane in 0..G {
                let Some(&i) = group.get(lane) else {
                    ids.push(u32::MAX);
                    continue;
                };
                ids.push((id0 + i) as u32);
                starts[i] = (base + lane) as u32;
                for (w, chunk) in paths[i].as_bytes().chunks(4).enumerate() {
                    let mut bytes = [0u8; 4];
                    bytes[..chunk.len()].copy_from_slice(chunk);
                    soa[base + w * G + lane] = u32::from_le_bytes(bytes);
                }
            }
        }
    }
    (soa, starts, ids)
}

fn grid(groups: u32) -> (u32, u32) {
    let x = groups.clamp(1, 32768);
    (x, groups.div_ceil(x))
}

// Word-aligned paths starting at word `word0`; a trailing zero word lets the
// DP read one word past the last path.
fn pack_paths(paths: &[String], word0: usize) -> (Vec<u32>, Vec<[u32; 2]>) {
    let total: usize = paths.iter().map(|p| p.len() + 3).sum();
    let mut bytes = Vec::with_capacity(total + 4);
    let mut items = Vec::with_capacity(paths.len());
    for p in paths {
        items.push([(word0 * 4 + bytes.len()) as u32, p.len() as u32]);
        bytes.extend_from_slice(p.as_bytes());
        bytes.resize(bytes.len().div_ceil(4) * 4, 0);
    }
    bytes.resize(bytes.len().div_ceil(4) * 4 + 4, 0);
    let words = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    (words, items)
}
