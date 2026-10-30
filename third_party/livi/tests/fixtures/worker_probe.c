/* Checks that the host never calls restore() while work() runs (LV2 puts
 * restore() in the Instantiation threading class). run() schedules one slow
 * piece of work; port 0 reads 1 while work() is running, and restore() sets
 * port 1 to 1 if it was called during work(). */
#include <lv2/core/lv2.h>
#include <lv2/state/state.h>
#include <lv2/worker/worker.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct {
    LV2_Worker_Schedule* schedule;
    volatile float* busy;
    volatile float* overlap;
    atomic_int in_work;
    int scheduled;
} Probe;

static LV2_Handle instantiate(const LV2_Descriptor* d, double rate,
                             const char* path, const LV2_Feature* const* features) {
    (void)d; (void)rate; (void)path;
    Probe* probe = calloc(1, sizeof(Probe));
    for (int i = 0; features && features[i]; ++i) {
        if (!strcmp(features[i]->URI, LV2_WORKER__schedule)) probe->schedule = features[i]->data;
    }
    return probe;
}
static void connect_port(LV2_Handle handle, uint32_t port, void* data) {
    Probe* probe = handle;
    if (port == 0) probe->busy = data;
    if (port == 1) probe->overlap = data;
}
static void run(LV2_Handle handle, uint32_t frames) {
    (void)frames;
    Probe* probe = handle;
    if (!probe->scheduled && probe->schedule) {
        probe->scheduled = 1;
        int token = 0;
        probe->schedule->schedule_work(probe->schedule->handle, sizeof token, &token);
    }
}
static LV2_Worker_Status work(LV2_Handle handle, LV2_Worker_Respond_Function respond,
                              LV2_Worker_Respond_Handle respond_handle, uint32_t size,
                              const void* data) {
    (void)respond; (void)respond_handle; (void)size; (void)data;
    Probe* probe = handle;
    atomic_store(&probe->in_work, 1);
    if (probe->busy) *probe->busy = 1.0f;
    struct timespec t = {0, 400 * 1000 * 1000};
    nanosleep(&t, NULL);
    if (probe->busy) *probe->busy = 0.0f;
    atomic_store(&probe->in_work, 0);
    return LV2_WORKER_SUCCESS;
}
static LV2_Worker_Status work_response(LV2_Handle handle, uint32_t size, const void* body) {
    (void)handle; (void)size; (void)body;
    return LV2_WORKER_SUCCESS;
}
static LV2_State_Status save(LV2_Handle handle, LV2_State_Store_Function store,
                             LV2_State_Handle state, uint32_t flags,
                             const LV2_Feature* const* features) {
    (void)handle; (void)store; (void)state; (void)flags; (void)features;
    return LV2_STATE_SUCCESS;
}
static LV2_State_Status restore(LV2_Handle handle, LV2_State_Retrieve_Function retrieve,
                                LV2_State_Handle state, uint32_t flags,
                                const LV2_Feature* const* features) {
    (void)retrieve; (void)state; (void)flags; (void)features;
    Probe* probe = handle;
    if (atomic_load(&probe->in_work) && probe->overlap) *probe->overlap = 1.0f;
    return LV2_STATE_SUCCESS;
}
static const void* extension_data(const char* uri) {
    static const LV2_Worker_Interface worker = {work, work_response, NULL};
    static const LV2_State_Interface state = {save, restore};
    if (!strcmp(uri, LV2_WORKER__interface)) return &worker;
    if (!strcmp(uri, LV2_STATE__interface)) return &state;
    return NULL;
}
static const LV2_Descriptor descriptor = {
    "urn:livi:test", instantiate, connect_port, NULL, run, NULL, free, extension_data
};
LV2_SYMBOL_EXPORT const LV2_Descriptor* lv2_descriptor(uint32_t index) {
    return index == 0 ? &descriptor : NULL;
}
