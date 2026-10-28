/*
 * Smoke test for libfff_c — the smallest possible end-to-end exercise of
 * the public C API. We:
 *
 *   1. Create a picker with an `FffCreateOptions` populated via C99
 *      designated initializers (the recommended idiom for direct C use).
 *   2. Wait for the initial scan to complete.
 *   3. Search for "smoke.c".
 *   4. Fail unless this very file appears in the results.
 *
 * Build + run via `make test-c-smoke`. Override $(CC) to test other
 * compilers.
 */

/* expose mkdtemp/usleep under -std=c99 on glibc; harmless on musl/darwin */
#define _DEFAULT_SOURCE
#define _BSD_SOURCE

#include <fff.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

// simple mock function to make sure that both globbing patterns and dir based pattern work
static int watch_glob_hits = 0;
static int watch_dir_hits = 0;
static int watch_all_hits = 0;
static int watch_ignored_leaks = 0;
static int watch_rename_hits = 0;
static int watch_rename_bad_source = 0;
static uint64_t watch_glob_id = 0;
static uint64_t watch_dir_id = 0;
static uint64_t watch_all_id = 0;

static void on_watch_batch(uint64_t watch_id, struct FffWatchEventBatch *batch, void *user_data) {
    (void)user_data;
    /* route by id like real SDKs do; unknown ids are benign no-ops */
    for (uint32_t i = 0; i < batch->count; i++) {
        const char *path = batch->events[i].path;
        if (!path) continue;
        if (watch_id == watch_glob_id && strstr(path, "hello.txt")) {
            watch_glob_hits++;
        }
        if (watch_id == watch_dir_id) {
            if (strstr(path, "hello.txt")) watch_dir_hits++;
            if (strstr(path, "noise.log")) watch_ignored_leaks++;
        }
        if (watch_id == watch_all_id && strstr(path, "hello.txt")) {
            watch_all_hits++;
        }
        /* kind 4 = renamed: the source rides in the parallel array */
        if (watch_id == watch_all_id && fff_watch_events_get_kind(batch, i) == 4 &&
            strstr(path, "renamed.txt")) {
            const char *from = fff_watch_events_get_from_path(batch, i);
            if (from && strstr(from, "to_rename.txt")) {
                watch_rename_hits++;
            } else {
                watch_rename_bad_source++;
            }
        }
    }

    fff_free_watch_events(batch); // need to clean dynamic array of events
}

static int watch_smoke(void) {
    char tmpl[] = "/tmp/fff-c-watch-XXXXXX";
    char *dir = mkdtemp(tmpl);
    if (!dir) {
        fprintf(stderr, "watch_smoke: mkdtemp failed\n");
        return 1;
    }

    struct FffResult *create_result = fff_create_instance_with(&(struct FffCreateOptions){
        .version = FFF_CREATE_OPTIONS_VERSION,
        .base_path = dir,
        .enable_mmap_cache = false,
        .enable_content_indexing = false,
        .watch = true,
    });
    if (!create_result->success) {
        fprintf(stderr, "watch_smoke: create failed: %s\n",
                create_result->error ? create_result->error : "?");
        fff_free_result(create_result);
        return 1;
    }
    void *picker = create_result->handle;
    fff_free_result(create_result);

    struct FffResult *r = fff_wait_for_scan(picker, 10000);
    fff_free_result(r);
    r = fff_wait_for_watcher(picker, 10000);
    fff_free_result(r);
    usleep(300 * 1000); /* let the FSEvents stream settle */

    /* instance-wide callback, then two subscriptions routed by id */
    r = fff_set_watch_callback(picker, on_watch_batch, NULL);
    if (!r->success) {
        fprintf(stderr, "watch_smoke: fff_set_watch_callback failed: %s\n",
                r->error ? r->error : "?");
        fff_free_result(r);
        fff_destroy(picker);
        return 1;
    }
    fff_free_result(r);

    r = fff_watch(picker, "**/*.txt", NULL);
    if (!r->success) {
        fprintf(stderr, "watch_smoke: fff_watch failed: %s\n", r->error ? r->error : "?");
        fff_free_result(r);
        fff_destroy(picker);
        return 1;
    }
    watch_glob_id = (uint64_t)r->int_value;
    fff_free_result(r);

    /* whole-tree dir subscription with an ignore glob */
    const char *ignores[] = {"*.log"};
    r = fff_watch(picker, dir,
                  &(struct FffWatchOptions){.version = FFF_WATCH_OPTIONS_VERSION,
                                            .ignore = ignores,
                                            .ignore_count = 1});
    if (!r->success) {
        fprintf(stderr, "watch_smoke: dir fff_watch failed: %s\n", r->error ? r->error : "?");
        fff_free_result(r);
        fff_destroy(picker);
        return 1;
    }
    watch_dir_id = (uint64_t)r->int_value;
    fff_free_result(r);

    /* NULL pattern subscribes to the entire indexed tree */
    r = fff_watch(picker, NULL, NULL);
    if (!r->success) {
        fprintf(stderr, "watch_smoke: NULL-pattern fff_watch failed: %s\n",
                r->error ? r->error : "?");
        fff_free_result(r);
        fff_destroy(picker);
        return 1;
    }
    watch_all_id = (uint64_t)r->int_value;
    fff_free_result(r);

    char file_path[512];
    snprintf(file_path, sizeof(file_path), "%s/hello.txt", dir);
    FILE *f = fopen(file_path, "w");
    if (!f) {
        fprintf(stderr, "watch_smoke: fopen failed\n");
        fff_destroy(picker);
        return 1;
    }
    fputs("hello watch\n", f);
    fclose(f);

    /* must be filtered out by the dir subscription's ignore glob */
    char log_path[512];
    snprintf(log_path, sizeof(log_path), "%s/noise.log", dir);
    FILE *lf = fopen(log_path, "w");
    if (lf) {
        fputs("noise\n", lf);
        fclose(lf);
    }

    for (int attempt = 0;
         attempt < 100 && (watch_glob_hits == 0 || watch_dir_hits == 0 || watch_all_hits == 0);
         attempt++) {
        usleep(100 * 1000);
    }

    /* a rename must arrive as one kind-4 event carrying both paths */
    char rename_src[512];
    char rename_dst[512];
    snprintf(rename_src, sizeof(rename_src), "%s/to_rename.txt", dir);
    snprintf(rename_dst, sizeof(rename_dst), "%s/renamed.txt", dir);
    FILE *rf = fopen(rename_src, "w");
    if (rf) {
        fputs("move me\n", rf);
        fclose(rf);
    }
    usleep(500 * 1000); /* let the create land before the move */
    if (rename(rename_src, rename_dst) != 0) {
        fprintf(stderr, "watch_smoke: rename failed\n");
        fff_destroy(picker);
        return 1;
    }
    for (int attempt = 0; attempt < 100 && watch_rename_hits == 0; attempt++) {
        usleep(100 * 1000);
    }

    r = fff_unwatch(picker, watch_glob_id);
    fff_free_result(r);
    r = fff_unwatch(picker, watch_dir_id);
    fff_free_result(r);
    r = fff_unwatch(picker, watch_all_id);
    fff_free_result(r);
    /* unwatch of an unknown id reports 0, not an error */
    r = fff_unwatch(picker, watch_dir_id);
    int unwatch_idempotent = r->success && r->int_value == 0;
    fff_free_result(r);

    /* fff_destroy is the quiescence barrier: after it returns the callback
     * will never run again and could be freed (ours is static). */
    fff_destroy(picker);

    if (watch_glob_hits == 0) {
        fprintf(stderr, "watch_smoke FAIL: glob subscriber saw no events\n");
        return 1;
    }
    if (watch_dir_hits == 0) {
        fprintf(stderr, "watch_smoke FAIL: dir subscriber saw no events\n");
        return 1;
    }
    if (watch_all_hits == 0) {
        fprintf(stderr, "watch_smoke FAIL: NULL-pattern subscriber saw no events\n");
        return 1;
    }
    if (watch_ignored_leaks > 0) {
        fprintf(stderr, "watch_smoke FAIL: ignore glob leaked %d events\n", watch_ignored_leaks);
        return 1;
    }
    if (!unwatch_idempotent) {
        fprintf(stderr, "watch_smoke FAIL: repeated unwatch was not a no-op\n");
        return 1;
    }
    if (watch_rename_hits == 0) {
        fprintf(stderr, "watch_smoke FAIL: no renamed event with a source path\n");
        return 1;
    }
    if (watch_rename_bad_source > 0) {
        fprintf(stderr, "watch_smoke FAIL: %d renamed events had a wrong source\n",
                watch_rename_bad_source);
        return 1;
    }

    fprintf(stderr, "watch_smoke PASS (glob=%d dir=%d all=%d rename=%d)\n", watch_glob_hits,
            watch_dir_hits, watch_all_hits, watch_rename_hits);
    return 0;
}

int main(int argc, char **argv) {
    const char *base_path = argc > 1 ? argv[1] : ".";

    // make sure that FFF C api is designed more for FFI rather than for direct C usage (I'm sorry)
    struct FffResult *create_result = fff_create_instance_with(&(struct FffCreateOptions){
        .version = FFF_CREATE_OPTIONS_VERSION,
        .base_path = base_path,
        .enable_mmap_cache = false,
        .enable_content_indexing = false,
        .watch = false,
    });

    if (!create_result->success) {
        fprintf(stderr, "fff couldn't create instance: %s\n",
                create_result->error ? create_result->error : "?");
        fff_free_result(create_result);
        return 1;
    }

    void *file_picker = create_result->handle;
    fff_free_result(create_result); // safe to drop now: handle outlives the envelope

    struct FffResult *scan_result = fff_wait_for_scan(file_picker, 5000);
    if (!scan_result->success) {
        fprintf(stderr, "wait_for_scan failed: %s\n",
                scan_result->error ? scan_result->error : "?");
        fff_free_result(scan_result);
        fff_destroy(file_picker);
        return 1;
    }
    // int_value: 1 = scan completed in time, 0 = timed out.
    if (scan_result->int_value == 0) {
        fprintf(stderr, "wait_for_scan: timed out before initial scan finished\n");
        fff_free_result(scan_result);
        fff_destroy(file_picker);
        return 1;
    }
    fff_free_result(scan_result);

    struct FffResult *res = fff_search(file_picker, "smkoe.c", "", 0, 0, 50, 0, 0);
    if (!res->success) {
        fprintf(stderr, "search failed: %s\n", res->error ? res->error : "?");
        fff_free_result(res);
        fff_destroy(file_picker);
        return 1;
    }

    struct FffSearchResult *sr = (struct FffSearchResult *)res->handle;
    uint32_t total = sr->count;
    int found = 0;
    for (uint32_t i = 0; i < sr->count; i++) {
        const char *path = sr->items[i].relative_path;
        if (path && strstr(path, "smoke.c")) {
            found = 1;
            fprintf(stderr, "found self: %s\n", path);
            break;
        }
    }

    fff_free_search_result(sr);
    fff_free_result(res);
    fff_destroy(file_picker);

    if (!found) {
        fprintf(stderr, "FAIL: smoke.c not in search results (count=%u)\n", total);
        return 1;
    }

    if (watch_smoke() != 0) {
        fprintf(stderr, "FAIL: watch test failed\n");
        return 1;
    }

    fprintf(stderr, "PASS\n");
    return 0;
}
