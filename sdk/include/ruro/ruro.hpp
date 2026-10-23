// ruro.hpp: writing ruro nodes in C++ (C++17 or newer), on top of ruro.h.
//
//     #include <ruro/ruro.hpp>
//
//     class Gain : public ruro::Node {
//     public:
//         static ruro::Spec spec() {
//             return ruro::Spec("acme/gain")
//                 .doc("Multiplies its input by k.")
//                 .input<ruro::Scalar>("input")
//                 .output<ruro::Scalar>("output")
//                 .param("k", 2.0, -10.0, 10.0, "the gain");
//         }
//         explicit Gain(ruro::Builder& b)
//             : in_(b.input<ruro::Scalar>("input")),
//               out_(b.output<ruro::Scalar>("output")),
//               k_(b.param<double>("k")) {}
//         void step(ruro::Ctx&) override {
//             while (auto s = in_.recv()) out_.send(ruro::Scalar{s->value * k_});
//         }
//         bool set_param(const std::string& name, const ruro::Value& v) override {
//             if (name != "k") return false;
//             k_ = v.f;
//             return true;
//         }
//     private:
//         ruro::Input<ruro::Scalar> in_;
//         ruro::Output<ruro::Scalar> out_;
//         double k_;
//     };
//
//     RURO_PLUGIN(Gain)
//
// Build it into a shared library:
//
//     c++ -std=c++17 -shared -fPIC -I<ruro>/sdk/include -o libacme.so gain.cpp
//
// An exception thrown by a constructor or a callback becomes the callback's
// error (a node whose constructor throws fails to start; a step that throws is
// counted and logged). The rules of ruro.h on threads and lifetimes apply.
#ifndef RURO_HPP
#define RURO_HPP

#include <cstring>
#include <exception>
#include <optional>
#include <stdexcept>
#include <type_traits>
#include <string>
#include <utility>
#include <vector>

#include "ruro.h"

namespace ruro {

// The messages are ruro.h's structs.
using Scalar = ruro_scalar;
using Text = ruro_text;
using Flag = ruro_flag;
using Bytes = ruro_bytes;
using Vec3 = ruro_vec3;
using Quat = ruro_quat;
using Pose = ruro_pose;
using Twist = ruro_twist;
using JointState = ruro_joint_state;
using Header = ruro_header;
using JointCommand = ruro_joint_command;
using Transform = ruro_transform;
using Transforms = ruro_transforms;
using Imu = ruro_imu;
using LaserScan = ruro_laser_scan;
using Odometry = ruro_odometry;
using CameraInfo = ruro_camera_info;
using Joy = ruro_joy;
using Image = ruro_image;
using PointCloud = ruro_point_cloud;

// The type name of each message struct.
template <class T>
struct MessageType;
#define RURO_MESSAGE_TYPE(T, NAME)                     \
    template <>                                        \
    struct MessageType<T> {                            \
        static constexpr const char *name = NAME;      \
    };
RURO_MESSAGE_TYPE(ruro_scalar, "std/Scalar")
RURO_MESSAGE_TYPE(ruro_text, "std/Text")
RURO_MESSAGE_TYPE(ruro_flag, "std/Flag")
RURO_MESSAGE_TYPE(ruro_bytes, "std/Bytes")
RURO_MESSAGE_TYPE(ruro_vec3, "std/Vec3")
RURO_MESSAGE_TYPE(ruro_pose, "std/Pose")
RURO_MESSAGE_TYPE(ruro_twist, "std/Twist")
RURO_MESSAGE_TYPE(ruro_joint_state, "std/JointState")
RURO_MESSAGE_TYPE(ruro_joint_command, "std/JointCommand")
RURO_MESSAGE_TYPE(ruro_transform, "std/Transform")
RURO_MESSAGE_TYPE(ruro_transforms, "std/Transforms")
RURO_MESSAGE_TYPE(ruro_imu, "std/Imu")
RURO_MESSAGE_TYPE(ruro_laser_scan, "std/LaserScan")
RURO_MESSAGE_TYPE(ruro_odometry, "std/Odometry")
RURO_MESSAGE_TYPE(ruro_camera_info, "std/CameraInfo")
RURO_MESSAGE_TYPE(ruro_joy, "std/Joy")
RURO_MESSAGE_TYPE(ruro_image, "std/Image")
RURO_MESSAGE_TYPE(ruro_point_cloud, "std/PointCloud")
#undef RURO_MESSAGE_TYPE

// Views of standard containers, for building messages to send. The
// container must outlive the send() call.
inline ruro_str str(const std::string &s) { return {s.data(), s.size()}; }
inline ruro_f64s view(const std::vector<double> &v) { return {v.data(), v.size()}; }
inline ruro_f32s view(const std::vector<float> &v) { return {v.data(), v.size()}; }
inline ruro_i32s view(const std::vector<int32_t> &v) { return {v.data(), v.size()}; }
inline ruro_u8s view(const std::vector<uint8_t> &v) { return {v.data(), v.size()}; }
inline std::string to_string(ruro_str s) { return s.ptr ? std::string(s.ptr, s.len) : std::string(); }

// A parameter value; `kind` (RURO_INT, RURO_FLOAT, RURO_BOOL, RURO_STRING)
// says which field holds it.
struct Value {
    uint32_t kind = RURO_INT;
    int64_t i = 0;
    double f = 0.0;
    bool b = false;
    std::string s;
};

namespace detail {
inline const ruro_api *&api() {
    static const ruro_api *a = nullptr;
    return a;
}

// Thrown after the host recorded the error itself (a failed send or a port
// it refused), so that the host's message is kept.
struct Recorded : std::exception {
    const char *what() const noexcept override { return "error recorded by the host"; }
};

inline Value from_c(const ruro_value &v) {
    Value out;
    out.kind = v.kind;
    out.i = v.i;
    out.f = v.kind == RURO_INT ? static_cast<double>(v.i) : v.f;
    out.b = v.b;
    out.s = to_string(v.s);
    return out;
}
}  // namespace detail

// Logging to the host's log.
inline void log_info(const std::string &m) { detail::api()->log(RURO_LOG_INFO, m.c_str()); }
inline void log_warn(const std::string &m) { detail::api()->log(RURO_LOG_WARN, m.c_str()); }
inline void log_error(const std::string &m) { detail::api()->log(RURO_LOG_ERROR, m.c_str()); }

// Timing and state of the current call.
class Ctx {
public:
    explicit Ctx(ruro_ctx *c) : c_(c) {}
    double dt() const { return detail::api()->dt(c_); }
    double time() const { return detail::api()->time(c_); }
    uint64_t steps() const { return detail::api()->steps(c_); }
    uint64_t now_ns() const { return detail::api()->now_ns(c_); }
    bool estopped() const { return detail::api()->estopped(c_); }
    ruro_ctx *raw() const { return c_; }

private:
    ruro_ctx *c_;
};

// A received message: the message's fields (it is the view, valid until the
// next receive on its input or the end of the call), its sequence number and
// send time.
template <class T>
struct Sample : T {
    uint64_t seq = 0;
    uint64_t stamp_ns = 0;
};

template <class T>
class Input {
public:
    Input() = default;
    explicit Input(ruro_input *h) : h_(h) {}
    // The oldest queued message, if any.
    std::optional<Sample<T>> recv() { return take(false); }
    // The newest queued message, if any; the older ones are dropped.
    std::optional<Sample<T>> latest() { return take(true); }
    size_t queued() const { return detail::api()->queued(h_); }

private:
    std::optional<Sample<T>> take(bool newest) {
        Sample<T> s{};
        T *view = &s;
        ruro_sample info{};
        int r = newest ? detail::api()->latest(h_, view, &info)
                       : detail::api()->recv(h_, view, &info);
        if (r < 0) throw detail::Recorded();
        if (r == 0) return std::nullopt;
        s.seq = info.seq;
        s.stamp_ns = info.stamp_ns;
        return s;
    }
    ruro_input *h_ = nullptr;
};

template <class T>
class Output {
public:
    Output() = default;
    explicit Output(ruro_output *h) : h_(h) {}
    // Sends `msg`; the host copies it before returning.
    void send(const T &msg) {
        if (detail::api()->send(h_, &msg) != 0) throw detail::Recorded();
    }

private:
    ruro_output *h_ = nullptr;
};

class Spec;

// Hands a node its ports and parameters while it is constructed.
class Builder {
public:
    Builder(ruro_builder *b, const Spec *spec) : b_(b), spec_(spec) {}
    std::string id() const { return detail::api()->node_id(b_); }

    template <class T>
    Input<T> input(const char *name);
    template <class T>
    Output<T> output(const char *name);

    // The parameter's value as the graph sets it (or its default):
    // param<double>, param<int64_t>, param<int>, param<bool>, param<std::string>.
    template <class V>
    V param(const char *name) const {
        ruro_value v{};
        if (!detail::api()->param(b_, name, &v))
            throw std::invalid_argument(std::string("no parameter `") + name + "`");
        Value val = detail::from_c(v);
        if constexpr (std::is_same_v<V, bool>) {
            return val.b;
        } else if constexpr (std::is_same_v<V, std::string>) {
            return val.s;
        } else if constexpr (std::is_integral_v<V>) {
            return static_cast<V>(val.kind == RURO_INT ? val.i : static_cast<int64_t>(val.f));
        } else {
            return static_cast<V>(val.f);
        }
    }

    // The full value of a parameter.
    Value value(const char *name) const {
        ruro_value v{};
        if (!detail::api()->param(b_, name, &v))
            throw std::invalid_argument(std::string("no parameter `") + name + "`");
        return detail::from_c(v);
    }

private:
    ruro_builder *b_;
    const Spec *spec_;
};

// The node interface. The constructor takes a Builder.
class Node {
public:
    virtual ~Node() = default;
    virtual void start(Ctx &) {}
    virtual void step(Ctx &ctx) = 0;
    virtual void stop(Ctx &) {}
    virtual void on_estop(Ctx &) {}
    // A parameter changed while the graph runs (already range-checked).
    // Return false to refuse it; by default changes are refused.
    virtual bool set_param(const std::string &name, const Value &value) {
        (void)name;
        (void)value;
        return false;
    }
};

// A node type's description: kind, ports and parameters.
class Spec {
public:
    explicit Spec(std::string kind) : kind_(std::move(kind)) {}
    Spec &doc(std::string d) {
        doc_ = std::move(d);
        return *this;
    }
    // Default trigger rate in Hz; without one the node steps on input.
    Spec &rate(double hz) {
        rate_ = hz;
        return *this;
    }
    template <class T>
    Spec &input(std::string name) {
        inputs_.push_back({std::move(name), MessageType<T>::name});
        return *this;
    }
    template <class T>
    Spec &output(std::string name) {
        outputs_.push_back({std::move(name), MessageType<T>::name});
        return *this;
    }
    Spec &param(std::string name, double def, std::optional<double> min = std::nullopt,
                std::optional<double> max = std::nullopt, std::string doc = "") {
        ruro_value v{};
        v.kind = RURO_FLOAT;
        v.f = def;
        return add(std::move(name), v, "", min, max, std::move(doc));
    }
    Spec &param(std::string name, int64_t def, std::optional<double> min = std::nullopt,
                std::optional<double> max = std::nullopt, std::string doc = "") {
        ruro_value v{};
        v.kind = RURO_INT;
        v.i = def;
        return add(std::move(name), v, "", min, max, std::move(doc));
    }
    Spec &param(std::string name, int def, std::optional<double> min = std::nullopt,
                std::optional<double> max = std::nullopt, std::string doc = "") {
        return param(std::move(name), static_cast<int64_t>(def), min, max, std::move(doc));
    }
    Spec &param(std::string name, bool def, std::string doc = "") {
        ruro_value v{};
        v.kind = RURO_BOOL;
        v.b = def;
        return add(std::move(name), v, "", std::nullopt, std::nullopt, std::move(doc));
    }
    Spec &param(std::string name, const char *def, std::string doc = "") {
        ruro_value v{};
        v.kind = RURO_STRING;
        return add(std::move(name), v, def, std::nullopt, std::nullopt, std::move(doc));
    }

    // The declared type of a port, or nullptr.
    const char *input_type(const char *name) const { return find(inputs_, name); }
    const char *output_type(const char *name) const { return find(outputs_, name); }

    // The C description; valid while this Spec lives and is not changed.
    const ruro_node_type *c_type(const ruro_node_type &callbacks) {
        cin_.clear();
        cout_.clear();
        cpar_.clear();
        for (auto &p : inputs_) cin_.push_back({p.first.c_str(), p.second});
        for (auto &p : outputs_) cout_.push_back({p.first.c_str(), p.second});
        for (auto &p : params_) {
            ruro_param_desc d{};
            d.name = p.name.c_str();
            d.default_value = p.def;
            if (p.def.kind == RURO_STRING) d.default_value.s = str(p.sdef);
            d.has_min = p.min.has_value();
            d.min = p.min.value_or(0.0);
            d.has_max = p.max.has_value();
            d.max = p.max.value_or(0.0);
            d.doc = p.doc.c_str();
            cpar_.push_back(d);
        }
        type_ = callbacks;
        type_.kind = kind_.c_str();
        type_.doc = doc_.c_str();
        type_.rate = rate_;
        type_.inputs = cin_.data();
        type_.n_inputs = cin_.size();
        type_.outputs = cout_.data();
        type_.n_outputs = cout_.size();
        type_.params = cpar_.data();
        type_.n_params = cpar_.size();
        type_.type_data = this;
        return &type_;
    }

    const std::string &kind() const { return kind_; }

private:
    struct Param {
        std::string name;
        ruro_value def;
        std::string sdef;
        std::optional<double> min, max;
        std::string doc;
    };
    Spec &add(std::string name, ruro_value def, std::string sdef, std::optional<double> min,
              std::optional<double> max, std::string doc) {
        params_.push_back({std::move(name), def, std::move(sdef), min, max, std::move(doc)});
        return *this;
    }
    static const char *find(const std::vector<std::pair<std::string, const char *>> &ports,
                            const char *name) {
        for (auto &p : ports)
            if (p.first == name) return p.second;
        return nullptr;
    }

    std::string kind_, doc_;
    double rate_ = 0.0;
    std::vector<std::pair<std::string, const char *>> inputs_, outputs_;
    std::vector<Param> params_;
    std::vector<ruro_port_desc> cin_, cout_;
    std::vector<ruro_param_desc> cpar_;
    ruro_node_type type_{};
};

template <class T>
Input<T> Builder::input(const char *name) {
    const char *declared = spec_->input_type(name);
    if (declared && std::strcmp(declared, MessageType<T>::name) != 0)
        throw std::invalid_argument(std::string("input `") + name + "` is " + declared +
                                    ", not " + MessageType<T>::name);
    ruro_input *h = detail::api()->input(b_, name);
    if (!h) throw detail::Recorded();
    return Input<T>(h);
}

template <class T>
Output<T> Builder::output(const char *name) {
    const char *declared = spec_->output_type(name);
    if (declared && std::strcmp(declared, MessageType<T>::name) != 0)
        throw std::invalid_argument(std::string("output `") + name + "` is " + declared +
                                    ", not " + MessageType<T>::name);
    ruro_output *h = detail::api()->output(b_, name);
    if (!h) throw detail::Recorded();
    return Output<T>(h);
}

namespace detail {
// Runs `f`; an exception becomes the call's error. Nothing propagates into
// the host.
template <class F>
int guard(F &&f) noexcept {
    try {
        f();
        return 0;
    } catch (const Recorded &) {
        return 1;
    } catch (const std::exception &e) {
        api()->set_error(e.what());
        return 1;
    } catch (...) {
        api()->set_error("an exception that is not a std::exception");
        return 1;
    }
}

template <class N>
struct Glue {
    static void *create(const ruro_api *a, ruro_builder *b, void *type_data) noexcept {
        api() = a;
        void *node = nullptr;
        guard([&] {
            Builder builder(b, static_cast<const Spec *>(type_data));
            node = static_cast<Node *>(new N(builder));
        });
        return node;
    }
    static Node *self(void *p) { return static_cast<Node *>(p); }
    static int start(void *p, ruro_ctx *c) noexcept {
        return guard([&] {
            Ctx ctx(c);
            self(p)->start(ctx);
        });
    }
    static int step(void *p, ruro_ctx *c) noexcept {
        return guard([&] {
            Ctx ctx(c);
            self(p)->step(ctx);
        });
    }
    static void stop(void *p, ruro_ctx *c) noexcept {
        guard([&] {
            Ctx ctx(c);
            self(p)->stop(ctx);
        });
    }
    static void on_estop(void *p, ruro_ctx *c) noexcept {
        guard([&] {
            Ctx ctx(c);
            self(p)->on_estop(ctx);
        });
    }
    static int set_param(void *p, const char *name, const ruro_value *v) noexcept {
        return guard([&] {
            if (!self(p)->set_param(name, from_c(*v)))
                throw std::runtime_error(std::string("this node does not take changes to `") +
                                         name + "` while it runs");
        });
    }
    static void destroy(void *p) noexcept {
        try {
            delete self(p);
        } catch (...) {
        }
    }
};

template <class N>
int register_node(const ruro_api *a, ruro_registrar *reg) noexcept {
    int rc = 1;
    guard([&] {
        static Spec spec = N::spec();
        ruro_node_type callbacks{};
        callbacks.create = &Glue<N>::create;
        callbacks.start = &Glue<N>::start;
        callbacks.step = &Glue<N>::step;
        callbacks.stop = &Glue<N>::stop;
        callbacks.set_param = &Glue<N>::set_param;
        callbacks.on_estop = &Glue<N>::on_estop;
        callbacks.destroy = &Glue<N>::destroy;
        rc = ruro_register(a, reg, spec.c_type(callbacks));
    });
    return rc;
}

template <class... Ns>
int register_all(const ruro_api *a, ruro_registrar *reg) noexcept {
    api() = a;
    int failed = 0;
    ((failed |= register_node<Ns>(a, reg)), ...);
    return failed;
}
}  // namespace detail

}  // namespace ruro

// Defines the plugin's entry point, registering each node class given.
#define RURO_PLUGIN(...)                                         \
    RURO_PLUGIN_INIT(ruro_api_, ruro_reg_) {                     \
        return ::ruro::detail::register_all<__VA_ARGS__>(ruro_api_, ruro_reg_); \
    }

#endif  // RURO_HPP
