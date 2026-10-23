/* Test plugin in C (C11): "ctest/echo" sends back every message it receives,
 * on an input and an output of every standard type, which checks the struct
 * layouts both ways; "ctest/probe" reports its parameters and timing. */
#include <ruro/ruro.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ---- ctest/echo ------------------------------------------------------- */

#define TYPES(X)                                    \
    X(scalar, ruro_scalar, "std/Scalar")            \
    X(text, ruro_text, "std/Text")                  \
    X(flag, ruro_flag, "std/Flag")                  \
    X(bytes, ruro_bytes, "std/Bytes")               \
    X(vec3, ruro_vec3, "std/Vec3")                  \
    X(pose, ruro_pose, "std/Pose")                  \
    X(twist, ruro_twist, "std/Twist")               \
    X(joint_state, ruro_joint_state, "std/JointState") \
    X(joint_command, ruro_joint_command, "std/JointCommand") \
    X(transform, ruro_transform, "std/Transform")   \
    X(transforms, ruro_transforms, "std/Transforms") \
    X(imu, ruro_imu, "std/Imu")                     \
    X(laser_scan, ruro_laser_scan, "std/LaserScan") \
    X(odometry, ruro_odometry, "std/Odometry")      \
    X(camera_info, ruro_camera_info, "std/CameraInfo") \
    X(joy, ruro_joy, "std/Joy")                     \
    X(image, ruro_image, "std/Image")               \
    X(point_cloud, ruro_point_cloud, "std/PointCloud")

#define PORT(name, t, ty) {#name, ty},
static const ruro_port_desc echo_ports[] = {TYPES(PORT)};
#define N_TYPES (sizeof echo_ports / sizeof echo_ports[0])

typedef struct {
    const ruro_api *api;
    ruro_input *in[N_TYPES];
    ruro_output *out[N_TYPES];
} echo;

static void *echo_create(const ruro_api *api, ruro_builder *b, void *type_data) {
    (void)type_data;
    echo *e = calloc(1, sizeof *e);
    e->api = api;
    for (size_t i = 0; i < N_TYPES; i++) {
        e->in[i] = api->input(b, echo_ports[i].name);
        e->out[i] = api->output(b, echo_ports[i].name);
        if (!e->in[i] || !e->out[i]) {
            free(e);
            return NULL;
        }
    }
    return e;
}

/* The largest message struct, to receive any of them into. */
typedef union {
#define MEMBER(name, t, ty) t name;
    TYPES(MEMBER)
} any_message;

static int echo_step(void *self, ruro_ctx *ctx) {
    (void)ctx;
    echo *e = self;
    for (size_t i = 0; i < N_TYPES; i++) {
        any_message m;
        int r;
        while ((r = e->api->recv(e->in[i], &m, NULL)) == 1) {
            /* Strings in views end with a NUL. */
            if (i == 1 && m.text.text.ptr[m.text.text.len] != '\0') {
                e->api->set_error("a text view is not NUL-terminated");
                return 1;
            }
            if (e->api->send(e->out[i], &m) != 0) return 1;
        }
        if (r < 0) return 1;
    }
    return 0;
}

static void echo_destroy(void *self) { free(self); }

static const ruro_node_type echo_type = {
    .kind = "ctest/echo",
    .doc = "Sends back every message, of every standard type.",
    .inputs = echo_ports,
    .n_inputs = N_TYPES,
    .outputs = echo_ports,
    .n_outputs = N_TYPES,
    .create = echo_create,
    .step = echo_step,
    .destroy = echo_destroy,
};

/* ---- ctest/probe ------------------------------------------------------ */

static const ruro_port_desc probe_inputs[] = {{"input", "std/Scalar"}};
static const ruro_port_desc probe_outputs[] = {{"report", "std/Text"}, {"image", "std/Image"}};
static const ruro_param_desc probe_params[] = {
    {.name = "count", .default_value = {.kind = RURO_INT, .i = 3}, .has_min = true, .min = 0,
     .has_max = true, .max = 100, .doc = "an integer"},
    {.name = "gain", .default_value = {.kind = RURO_FLOAT, .f = 1.5}, .doc = "a number"},
    {.name = "enabled", .default_value = {.kind = RURO_BOOL, .b = true}},
    {.name = "mode", .default_value = {.kind = RURO_STRING, .s = {"normal", 6}}},
};

typedef struct {
    const ruro_api *api;
    ruro_input *in;
    ruro_output *report, *image;
    char id[64];
    char mode[32];
    int64_t count;
    double gain;
    bool enabled;
    int estops;
} probe;

static void *probe_create(const ruro_api *api, ruro_builder *b, void *type_data) {
    (void)type_data;
    probe *p = calloc(1, sizeof *p);
    p->api = api;
    p->in = api->input(b, "input");
    p->report = api->output(b, "report");
    p->image = api->output(b, "image");
    ruro_value v;
    if (!api->param(b, "count", &v) || v.kind != RURO_INT) goto fail;
    p->count = v.i;
    if (!api->param(b, "gain", &v) || v.kind != RURO_FLOAT) goto fail;
    p->gain = v.f;
    if (!api->param(b, "enabled", &v) || v.kind != RURO_BOOL) goto fail;
    p->enabled = v.b;
    if (!api->param(b, "mode", &v) || v.kind != RURO_STRING) goto fail;
    snprintf(p->mode, sizeof p->mode, "%s", v.s.ptr);
    snprintf(p->id, sizeof p->id, "%s", api->node_id(b));
    if (api->param(b, "missing", &v)) goto fail;
    if (api->input(b, "undeclared") != NULL) goto fail;
    if (strcmp(p->mode, "fail-create") == 0) {
        api->set_error("refusing to be created");
        free(p);
        return NULL;
    }
    return p;
fail:
    api->set_error("parameters are not as declared");
    free(p);
    return NULL;
}

static int probe_start(void *self, ruro_ctx *ctx) {
    (void)ctx;
    probe *p = self;
    if (strcmp(p->mode, "fail-start") == 0) {
        p->api->set_error("refusing to start");
        return 1;
    }
    return 0;
}

static int probe_step(void *self, ruro_ctx *ctx) {
    probe *p = self;
    const ruro_api *api = p->api;
    double sum = 0;
    ruro_scalar s;
    ruro_sample info;
    uint64_t last_seq = 0;
    while (api->recv(p->in, &s, &info) == 1) {
        sum += s.value;
        last_seq = info.seq;
    }
    char text[512];
    snprintf(text, sizeof text,
             "id=%s count=%lld gain=%g enabled=%d mode=%s sum=%g seq=%llu steps=%llu "
             "dt_positive=%d estopped=%d estops=%d",
             p->id, (long long)p->count, p->gain, p->enabled, p->mode, sum,
             (unsigned long long)last_seq, (unsigned long long)api->steps(ctx),
             api->dt(ctx) > 0, api->estopped(ctx), p->estops);
    ruro_text t = {{text, strlen(text)}};
    if (api->send(p->report, &t) != 0) return 1;
    if (strcmp(p->mode, "bad-image") == 0) {
        static const uint8_t px[3] = {1, 2, 3};
        ruro_image img = {.width = 2, .height = 2, .encoding = RURO_RGB8, .data = {px, 3}};
        return api->send(p->image, &img);
    }
    if (p->gain < 0) {
        api->set_error("gain is negative");
        return 1;
    }
    return 0;
}

static int probe_set_param(void *self, const char *name, const ruro_value *value) {
    probe *p = self;
    if (strcmp(name, "gain") == 0) {
        p->gain = value->f;
        return 0;
    }
    if (strcmp(name, "count") == 0) {
        p->count = value->i;
        return 0;
    }
    p->api->set_error("only gain and count change while running");
    return 1;
}

static void probe_on_estop(void *self, ruro_ctx *ctx) {
    (void)ctx;
    ((probe *)self)->estops++;
}

static void probe_destroy(void *self) { free(self); }

static const ruro_node_type probe_type = {
    .kind = "ctest/probe",
    .doc = "Reports its parameters and timing.",
    .rate = 10,
    .inputs = probe_inputs,
    .n_inputs = 1,
    .outputs = probe_outputs,
    .n_outputs = 2,
    .params = probe_params,
    .n_params = 4,
    .create = probe_create,
    .start = probe_start,
    .step = probe_step,
    .set_param = probe_set_param,
    .on_estop = probe_on_estop,
    .destroy = probe_destroy,
};

RURO_PLUGIN_INIT(api, reg) {
    if (ruro_register(api, reg, &echo_type) != 0) return 1;
    return ruro_register(api, reg, &probe_type);
}
