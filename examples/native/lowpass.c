/* A ruro node in C: a first-order low-pass filter on std/Scalar.
 *
 *     cc -std=c11 -shared -fPIC -I../../sdk/include -o libexample_nodes.so lowpass.c
 *
 * or build both examples with CMake or make (see the book's "Nodes in C and
 * C++"), then start the daemon with RURO_PLUGINS=$PWD/libexample_nodes.so. */
#include <ruro/ruro.h>

#include <stdlib.h>

static const ruro_port_desc inputs[] = {{"input", "std/Scalar"}};
static const ruro_port_desc outputs[] = {{"output", "std/Scalar"}};
static const ruro_param_desc params[] = {
    {.name = "alpha",
     .default_value = {.kind = RURO_FLOAT, .f = 0.1},
     .has_min = true, .min = 0.0, .has_max = true, .max = 1.0,
     .doc = "weight of each new sample"},
};

typedef struct {
    const ruro_api *api;
    ruro_input *in;
    ruro_output *out;
    double alpha, state;
    bool primed;
} lowpass;

static void *create(const ruro_api *api, ruro_builder *b, void *type_data) {
    (void)type_data;
    lowpass *n = calloc(1, sizeof *n);
    if (!n) {
        api->set_error("out of memory");
        return NULL;
    }
    n->api = api;
    n->in = api->input(b, "input");
    n->out = api->output(b, "output");
    ruro_value v;
    api->param(b, "alpha", &v);
    n->alpha = v.f;
    if (!n->in || !n->out) {
        free(n);
        return NULL;
    }
    return n;
}

static int step(void *self, ruro_ctx *ctx) {
    (void)ctx;
    lowpass *n = self;
    ruro_scalar s;
    while (n->api->recv(n->in, &s, NULL) == 1) {
        n->state = n->primed ? n->state + n->alpha * (s.value - n->state) : s.value;
        n->primed = true;
        ruro_scalar out = {n->state};
        if (n->api->send(n->out, &out) != 0) return 1;
    }
    return 0;
}

static int set_param(void *self, const char *name, const ruro_value *value) {
    (void)name; /* alpha is the only parameter */
    ((lowpass *)self)->alpha = value->f;
    return 0;
}

static void destroy(void *self) { free(self); }

static const ruro_node_type lowpass_type = {
    .kind = "example/c-lowpass",
    .doc = "First-order low-pass filter, in C.",
    .inputs = inputs,
    .n_inputs = 1,
    .outputs = outputs,
    .n_outputs = 1,
    .params = params,
    .n_params = 1,
    .create = create,
    .step = step,
    .set_param = set_param,
    .destroy = destroy,
};

/* threshold.cpp registers its own node from the same library. */
int example_register_threshold(const ruro_api *api, ruro_registrar *reg);

RURO_PLUGIN_INIT(api, reg) {
    if (ruro_register(api, reg, &lowpass_type) != 0) return 1;
    return example_register_threshold(api, reg);
}
