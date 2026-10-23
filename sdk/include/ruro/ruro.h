/*
 * ruro.h: writing ruro nodes in C (and, through ruro.hpp, in C++).
 *
 * A plugin is a shared library that exports `ruro_plugin_init`. Hosts load the
 * libraries listed in RURO_PLUGINS when they start; the plugin registers its
 * node types there, and the host then runs them in its own process, next to
 * its Rust nodes, with the same ports, parameters, metrics and studio support.
 *
 *     #include <ruro/ruro.h>
 *
 *     RURO_PLUGIN_INIT(api, reg) {
 *         return ruro_register(api, reg, &my_node_type);
 *     }
 *
 * Build it as position-independent code into a shared library:
 *
 *     cc -shared -fPIC -I<ruro>/sdk/include -o libmy_nodes.so my_nodes.c
 *
 * Threads and lifetimes:
 * - Each node instance is called from one thread at a time: create, start,
 *   step, set_param, on_estop, stop and destroy never overlap for one
 *   instance. Different instances may run on different threads at once.
 * - Outputs may be sent only from within step (and start, stop, on_estop).
 * - A message received with recv() or latest() points into memory the host
 *   owns. It stays valid until the next recv()/latest() on the same input or
 *   the end of the call, whichever comes first. Numeric arrays and image data
 *   are not copied; strings are copied, and NUL-terminated.
 * - A message given to send() is copied by the host before send() returns.
 * - Strings in node types, ports and parameters (kind, names, docs) must stay
 *   valid while the plugin is loaded; string literals do.
 *
 * Errors: a callback that fails returns a nonzero value (create: NULL) after
 * calling api->set_error(). A failing start stops the node; a failing step is
 * counted and logged, and the node keeps running.
 */
#ifndef RURO_H
#define RURO_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped whenever a struct below or the meaning of a field changes. */
#define RURO_ABI_VERSION 1

/* ---- Views ------------------------------------------------------------ */

typedef struct ruro_str { const char *ptr; size_t len; } ruro_str;
typedef struct ruro_f64s { const double *ptr; size_t len; } ruro_f64s;
typedef struct ruro_f32s { const float *ptr; size_t len; } ruro_f32s;
typedef struct ruro_i32s { const int32_t *ptr; size_t len; } ruro_i32s;
typedef struct ruro_u8s { const uint8_t *ptr; size_t len; } ruro_u8s;

/* ---- The standard messages, one struct per type ----------------------- */

typedef struct ruro_scalar { double value; } ruro_scalar;               /* std/Scalar */
typedef struct ruro_text { ruro_str text; } ruro_text;                  /* std/Text */
typedef struct ruro_flag { bool value; } ruro_flag;                     /* std/Flag */
typedef struct ruro_bytes { ruro_u8s data; } ruro_bytes;                /* std/Bytes */
typedef struct ruro_vec3 { double x, y, z; } ruro_vec3;                 /* std/Vec3 */
typedef struct ruro_quat { double x, y, z, w; } ruro_quat;
typedef struct ruro_pose { ruro_vec3 position; ruro_quat orientation; } ruro_pose;  /* std/Pose */
typedef struct ruro_twist { ruro_vec3 linear, angular; } ruro_twist;    /* std/Twist */
typedef struct ruro_joint_state {                                       /* std/JointState */
    ruro_f64s position, velocity, effort;
} ruro_joint_state;
typedef struct ruro_header { uint64_t stamp_ns; ruro_str frame_id; } ruro_header;
typedef struct ruro_joint_command {                                     /* std/JointCommand */
    ruro_header header;
    ruro_f64s position, velocity, effort;
} ruro_joint_command;
typedef struct ruro_transform {                                         /* std/Transform */
    ruro_header header;
    ruro_str child_frame_id;
    ruro_vec3 translation;
    ruro_quat rotation;
} ruro_transform;
typedef struct ruro_transforms {                                        /* std/Transforms */
    const ruro_transform *ptr;
    size_t len;
} ruro_transforms;
typedef struct ruro_imu {                                               /* std/Imu */
    ruro_header header;
    ruro_quat orientation;
    double orientation_covariance[9];
    ruro_vec3 angular_velocity;
    double angular_velocity_covariance[9];
    ruro_vec3 linear_acceleration;
    double linear_acceleration_covariance[9];
} ruro_imu;
typedef struct ruro_laser_scan {                                        /* std/LaserScan */
    ruro_header header;
    float angle_min, angle_max, angle_increment, time_increment, scan_time;
    float range_min, range_max;
    ruro_f32s ranges, intensities;
} ruro_laser_scan;
typedef struct ruro_odometry {                                          /* std/Odometry */
    ruro_header header;
    ruro_str child_frame_id;
    ruro_pose pose;
    ruro_twist twist;
} ruro_odometry;
typedef struct ruro_camera_info {                                       /* std/CameraInfo */
    ruro_header header;
    uint32_t width, height;
    ruro_str distortion_model;
    ruro_f64s d;
    double k[9], r[9], p[12];
} ruro_camera_info;
typedef struct ruro_joy {                                               /* std/Joy */
    ruro_header header;
    ruro_f32s axes;
    ruro_i32s buttons;
} ruro_joy;

/* ruro_image.encoding */
#define RURO_RGB8 0u
#define RURO_RGBA8 1u
#define RURO_MONO8 2u
#define RURO_MONO16 3u  /* two bytes per pixel, native byte order */

typedef struct ruro_image {                                             /* std/Image */
    uint32_t width, height;
    uint32_t encoding;
    ruro_u8s data;  /* rows, top first, without padding */
} ruro_image;
typedef struct ruro_point_cloud {                                       /* std/PointCloud */
    const float *xyz;  /* x, y, z of each point: 3 * len floats */
    size_t len;
} ruro_point_cloud;

/* Sequence number and send time of a received message. */
typedef struct ruro_sample { uint64_t seq; uint64_t stamp_ns; } ruro_sample;

/* ---- Parameters ------------------------------------------------------- */

#define RURO_INT 0u
#define RURO_FLOAT 1u
#define RURO_BOOL 2u
#define RURO_STRING 3u

/* A parameter value: `kind` says which field holds it. */
typedef struct ruro_value {
    uint32_t kind;
    int64_t i;
    double f;
    bool b;
    ruro_str s;  /* NUL-terminated when the host provides it */
} ruro_value;

typedef struct ruro_param_desc {
    const char *name;
    ruro_value default_value;  /* its kind is the parameter's kind */
    bool has_min;
    double min;
    bool has_max;
    double max;
    const char *doc;
} ruro_param_desc;

typedef struct ruro_port_desc {
    const char *name;
    const char *type;  /* a message type name, e.g. "std/Image" */
} ruro_port_desc;

/* ---- Node types ------------------------------------------------------- */

typedef struct ruro_builder ruro_builder;  /* during create */
typedef struct ruro_ctx ruro_ctx;          /* during the other callbacks */
typedef struct ruro_input ruro_input;
typedef struct ruro_output ruro_output;
typedef struct ruro_registrar ruro_registrar;
typedef struct ruro_api ruro_api;

typedef struct ruro_node_type {
    const char *kind;  /* with a namespace: "acme/filter" */
    const char *doc;
    double rate;       /* default trigger rate in Hz; 0 steps on input */
    const ruro_port_desc *inputs;
    size_t n_inputs;
    const ruro_port_desc *outputs;
    size_t n_outputs;
    const ruro_param_desc *params;
    size_t n_params;

    /* Declares the instance's ports (api->input, api->output) and reads its
     * parameters (api->param); returns the instance, or NULL on error. */
    void *(*create)(const ruro_api *api, ruro_builder *b, void *type_data);
    int (*start)(void *self, ruro_ctx *ctx);        /* optional */
    int (*step)(void *self, ruro_ctx *ctx);
    void (*stop)(void *self, ruro_ctx *ctx);        /* optional */
    /* A parameter changed while the graph runs (already checked against its
     * range); nonzero refuses it. Optional: without it, changes are refused. */
    int (*set_param)(void *self, const char *name, const ruro_value *value);
    void (*on_estop)(void *self, ruro_ctx *ctx);    /* optional */
    void (*destroy)(void *self);
    void *type_data;   /* passed to create */
} ruro_node_type;

/* ---- What the host provides ------------------------------------------- */

#define RURO_LOG_ERROR 0
#define RURO_LOG_WARN 1
#define RURO_LOG_INFO 2
#define RURO_LOG_DEBUG 3

struct ruro_api {
    uint32_t size;         /* sizeof(ruro_api) on the host: fields are only added at the end */
    uint32_t abi_version;  /* RURO_ABI_VERSION of the host */

    /* Records the error of the current callback. */
    void (*set_error)(const char *message);
    void (*log)(int level, const char *message);

    /* create: the node's id in the graph, its ports and parameters. */
    const char *(*node_id)(ruro_builder *b);
    ruro_input *(*input)(ruro_builder *b, const char *name);    /* NULL if not declared */
    ruro_output *(*output)(ruro_builder *b, const char *name);  /* NULL if not declared */
    /* The value the graph sets, or the default; returns 0 if `name` is not a
     * parameter of the type. */
    int (*param)(ruro_builder *b, const char *name, ruro_value *out);

    /* Timing and state of the current step. */
    double (*dt)(ruro_ctx *ctx);      /* seconds since the previous step */
    double (*time)(ruro_ctx *ctx);    /* seconds since the node started */
    uint64_t (*steps)(ruro_ctx *ctx);
    uint64_t (*now_ns)(ruro_ctx *ctx);  /* graph time */
    bool (*estopped)(ruro_ctx *ctx);

    /* Inputs: `view` points to the struct of the input's message type.
     * recv takes the oldest queued message, latest the newest (dropping the
     * rest). They return 1 with a message, 0 with none; `sample` may be NULL. */
    size_t (*queued)(ruro_input *in);
    int (*recv)(ruro_input *in, void *view, ruro_sample *sample);
    int (*latest)(ruro_input *in, void *view, ruro_sample *sample);

    /* Outputs: `msg` points to the struct of the output's message type.
     * Returns 0, or nonzero (with the error recorded) if it is not valid. */
    int (*send)(ruro_output *out, const void *msg);

    /* Registration, from ruro_plugin_init; use ruro_register(). */
    int (*register_node)(ruro_registrar *reg, uint32_t abi_version, const uint32_t *layout,
                         size_t n_layout, const ruro_node_type *type);
};

/* The sizes of the structs above, as this plugin was compiled: the host
 * refuses a plugin whose layout differs from its own. */
#define RURO_LAYOUT                                                              \
    {                                                                            \
        sizeof(ruro_api), sizeof(ruro_str), sizeof(ruro_scalar), sizeof(ruro_text), \
            sizeof(ruro_flag), sizeof(ruro_bytes), sizeof(ruro_vec3),            \
            sizeof(ruro_quat), sizeof(ruro_pose), sizeof(ruro_twist),            \
            sizeof(ruro_joint_state), sizeof(ruro_header),                       \
            sizeof(ruro_joint_command), sizeof(ruro_transform),                  \
            sizeof(ruro_transforms), sizeof(ruro_imu), sizeof(ruro_laser_scan),  \
            sizeof(ruro_odometry), sizeof(ruro_camera_info), sizeof(ruro_joy),   \
            sizeof(ruro_image), sizeof(ruro_point_cloud), sizeof(ruro_sample),   \
            sizeof(ruro_value), sizeof(ruro_param_desc), sizeof(ruro_port_desc), \
            sizeof(ruro_node_type)                                               \
    }

/* Registers a node type; call it from ruro_plugin_init. Returns 0, or nonzero
 * if the host refuses the type (the reason is in the host's log). */
static inline int ruro_register(const ruro_api *api, ruro_registrar *reg,
                                const ruro_node_type *type) {
    static const uint32_t layout[] = RURO_LAYOUT;
    return api->register_node(reg, RURO_ABI_VERSION, layout, sizeof layout / sizeof layout[0],
                              type);
}

/* Defines the plugin's entry point:
 *
 *     RURO_PLUGIN_INIT(api, reg) { return ruro_register(api, reg, &t); }
 */
#if defined(_WIN32)
#define RURO_EXPORT __declspec(dllexport)
#else
#define RURO_EXPORT __attribute__((visibility("default")))
#endif
#ifdef __cplusplus
#define RURO_PLUGIN_INIT(api, reg) \
    extern "C" RURO_EXPORT int ruro_plugin_init(const ruro_api *api, ruro_registrar *reg)
#else
#define RURO_PLUGIN_INIT(api, reg) \
    RURO_EXPORT int ruro_plugin_init(const ruro_api *api, ruro_registrar *reg)
#endif

#ifdef __cplusplus
}
#endif

#endif /* RURO_H */
