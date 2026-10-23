/* Plugins the host must refuse: built with -DCASE=1 (another ABI version),
 * 2 (a struct of another size) or 3 (no entry point). */
#include <ruro/ruro.h>

#include <stdlib.h>

static void *create(const ruro_api *api, ruro_builder *b, void *d) {
    (void)api, (void)b, (void)d;
    return malloc(1);
}
static int step(void *self, ruro_ctx *ctx) {
    (void)self, (void)ctx;
    return 0;
}
static void destroy(void *self) { free(self); }

static const ruro_node_type t = {
    .kind = "refused/node", .create = create, .step = step, .destroy = destroy};

#if CASE == 1 || CASE == 2
RURO_PLUGIN_INIT(api, reg) {
    uint32_t layout[] = RURO_LAYOUT;
#if CASE == 1
    return api->register_node(reg, RURO_ABI_VERSION + 1, layout, sizeof layout / sizeof *layout, &t);
#else
    layout[15] += 8; /* as if ruro_imu had another field */
    return api->register_node(reg, RURO_ABI_VERSION, layout, sizeof layout / sizeof *layout, &t);
#endif
}
#else
RURO_EXPORT int not_the_entry_point(void) { return t.rate > 0; }
#endif
