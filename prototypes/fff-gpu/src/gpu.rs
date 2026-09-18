use bytemuck::{Pod, Zeroable};
use std::cell::RefCell;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

const SLOTS: u32 = 1;

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
    _pad: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kernel {
    ThreadPerItem,
    LanePerThread,
    Subgroup,
}

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
}

pub struct GpuMatcher {
    device: wgpu::Device,
    queue: wgpu::Queue,
    kernel: Kernel,
    score_pass: Pass,
    subgroup_passes: RefCell<HashMap<usize, Pass>>,
    subgroup_unsupported: RefCell<std::collections::HashSet<usize>>,
    survivor_entries: Vec<wgpu::Buffer>,
    prefilter_pass: Option<Pass>,
    args_pass: Option<Pass>,
    params_buf: wgpu::Buffer,
    needle_buf: wgpu::Buffer,
    count_buf: wgpu::Buffer,
    indirect_buf: wgpu::Buffer,
    scores_buf: wgpu::Buffer,
    staging_buf: wgpu::Buffer,
    item_count: u32,
    pub adapter_name: String,
}

impl GpuMatcher {
    pub fn new(paths: &[String], kernel: Kernel) -> Self {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("no GPU adapter");
        let adapter_name = format!(
            "{} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: adapter.features() & wgpu::Features::SUBGROUP,
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .expect("device");

        let (hay, items) = pack_paths(paths);
        let item_count = paths.len() as u32;

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let needle_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("needle"),
            size: 64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let hay_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("hay"),
            contents: bytemuck::cast_slice(&hay),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let items_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("items"),
            contents: bytemuck::cast_slice(&items),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let scores_size = (item_count.max(1) as u64) * 4;
        let scores_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scores"),
            size: scores_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: scores_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let survivors_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("survivors"),
            size: (item_count.max(1) as u64) * 16,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let count_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("count"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let indirect_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("indirect"),
            size: 12,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDIRECT,
            mapped_at_creation: false,
        });

        let make_pass = |label: &str, source: &str, bufs: &[&wgpu::Buffer]| {
            make_pass(&device, label, source, bufs)
        };
        let common = [&params_buf, &needle_buf, &hay_buf, &items_buf, &scores_buf];
        let with_survivors = [
            &params_buf,
            &needle_buf,
            &hay_buf,
            &items_buf,
            &scores_buf,
            &survivors_buf,
            &count_buf,
        ];

        let (score_pass, prefilter_pass, args_pass) = match kernel {
            Kernel::ThreadPerItem => (
                make_pass("score", include_str!("score.wgsl"), &common),
                None,
                None,
            ),
            Kernel::LanePerThread | Kernel::Subgroup => (
                make_pass("score", include_str!("score_lanes.wgsl"), &with_survivors),
                Some(make_pass(
                    "prefilter",
                    include_str!("prefilter.wgsl"),
                    &with_survivors,
                )),
                Some(make_pass(
                    "args",
                    include_str!("args.wgsl"),
                    &[&count_buf, &indirect_buf],
                )),
            ),
        };
        let survivor_entries = vec![
            params_buf.clone(),
            needle_buf.clone(),
            hay_buf.clone(),
            items_buf.clone(),
            scores_buf.clone(),
            survivors_buf.clone(),
            count_buf.clone(),
        ];

        Self {
            device,
            queue,
            kernel,
            score_pass,
            subgroup_passes: RefCell::new(HashMap::new()),
            subgroup_unsupported: RefCell::new(Default::default()),
            survivor_entries,
            prefilter_pass,
            args_pass,
            params_buf,
            needle_buf,
            count_buf,
            indirect_buf,
            scores_buf,
            staging_buf,
            item_count,
            adapter_name,
        }
    }

    // Full round trip: upload needle, dispatch, read back one score per item.
    // Subgroup kernel falls back to the barrier kernel when the driver picks
    // a subgroup narrower than 16 lanes for this needle length.
    pub fn search(&self, needle: &str, scoring: &neo_frizbee::Scoring) -> Vec<u32> {
        let n = needle.len().min(32);
        let use_subgroup =
            self.kernel == Kernel::Subgroup && !self.subgroup_unsupported.borrow().contains(&n);
        let scores = self.search_with(needle, scoring, use_subgroup);
        if use_subgroup && scores.contains(&u32::MAX) {
            self.subgroup_unsupported.borrow_mut().insert(n);
            return self.search_with(needle, scoring, false);
        }
        scores
    }

    fn search_with(
        &self,
        needle: &str,
        scoring: &neo_frizbee::Scoring,
        use_subgroup: bool,
    ) -> Vec<u32> {
        let needle_bytes = &needle.as_bytes()[..needle.len().min(32)];
        let mut packed = [0u8; 64];
        packed[..needle_bytes.len()].copy_from_slice(needle_bytes);

        let params = Params {
            needle_len: needle_bytes.len() as u32,
            item_count: self.item_count,
            match_score: scoring.match_score as u32,
            mismatch_penalty: scoring.mismatch_penalty as u32,
            gap_open: scoring.gap_open_penalty as u32,
            gap_extend: scoring.gap_extend_penalty as u32,
            prefix_bonus: scoring.prefix_bonus as u32,
            delimiter_bonus: scoring.delimiter_bonus as u32,
            capitalization_bonus: scoring.capitalization_bonus as u32,
            matching_case_bonus: scoring.matching_case_bonus as u32,
            exact_match_bonus: scoring.exact_match_bonus as u32,
            _pad: 0,
        };
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));
        self.queue.write_buffer(&self.needle_buf, 0, &packed);
        self.queue.write_buffer(&self.count_buf, 0, &[0u8; 4]);

        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            match self.kernel {
                Kernel::ThreadPerItem => {
                    let (x, y) = grid(self.item_count.div_ceil(64));
                    pass.set_pipeline(&self.score_pass.pipeline);
                    pass.set_bind_group(0, &self.score_pass.bind_group, &[]);
                    pass.dispatch_workgroups(x, y, 1);
                }
                Kernel::LanePerThread | Kernel::Subgroup => {
                    let mut cache = self.subgroup_passes.borrow_mut();
                    let score_pass = if use_subgroup {
                        cache.entry(needle_bytes.len()).or_insert_with(|| {
                            let bufs: Vec<&wgpu::Buffer> = self.survivor_entries.iter().collect();
                            make_pass(
                                &self.device,
                                "subgroup",
                                &crate::subgroup::generate(needle_bytes.len()),
                                &bufs,
                            )
                        })
                    } else {
                        &self.score_pass
                    };
                    let prefilter = self.prefilter_pass.as_ref().unwrap();
                    let args = self.args_pass.as_ref().unwrap();
                    let (x, y) = grid(self.item_count.div_ceil(64));
                    pass.set_pipeline(&prefilter.pipeline);
                    pass.set_bind_group(0, &prefilter.bind_group, &[]);
                    pass.dispatch_workgroups(x, y, 1);
                    pass.set_pipeline(&args.pipeline);
                    pass.set_bind_group(0, &args.bind_group, &[]);
                    pass.dispatch_workgroups(1, 1, 1);
                    pass.set_pipeline(&score_pass.pipeline);
                    pass.set_bind_group(0, &score_pass.bind_group, &[]);
                    pass.dispatch_workgroups_indirect(&self.indirect_buf, 0);
                }
            }
        }
        encoder.copy_buffer_to_buffer(
            &self.scores_buf,
            0,
            &self.staging_buf,
            0,
            self.staging_buf.size(),
        );
        self.queue.submit([encoder.finish()]);

        let slice = self.staging_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        rx.recv().unwrap().expect("map");
        let scores: Vec<u32> =
            bytemuck::cast_slice(&slice.get_mapped_range().expect("range")).to_vec();
        self.staging_buf.unmap();
        scores
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

fn grid(groups: u32) -> (u32, u32) {
    let x = groups.clamp(1, 32768);
    (x, groups.div_ceil(x))
}

fn pack_paths(paths: &[String]) -> (Vec<u32>, Vec<[u32; 2]>) {
    let total: usize = paths.iter().map(|p| p.len()).sum();
    let mut bytes = Vec::with_capacity(total + 4);
    let mut items = Vec::with_capacity(paths.len());
    for p in paths {
        items.push([bytes.len() as u32, p.len() as u32]);
        bytes.extend_from_slice(p.as_bytes());
    }
    bytes.resize(bytes.len().div_ceil(4) * 4 + 4, 0);
    let words = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    (words, items)
}
