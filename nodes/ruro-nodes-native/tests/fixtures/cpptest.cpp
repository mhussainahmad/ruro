// Test plugin in C++17: the Gain node of ruro.hpp's documentation, an image
// inverter, and a node whose constructor or step throws.
#include <ruro/ruro.hpp>

#include <vector>

class Gain : public ruro::Node {
public:
    static ruro::Spec spec() {
        return ruro::Spec("cpptest/gain")
            .doc("Multiplies its input by k.")
            .input<ruro::Scalar>("input")
            .output<ruro::Scalar>("output")
            .param("k", 2.0, -10.0, 10.0, "the gain");
    }
    explicit Gain(ruro::Builder &b)
        : in_(b.input<ruro::Scalar>("input")),
          out_(b.output<ruro::Scalar>("output")),
          k_(b.param<double>("k")) {}
    void step(ruro::Ctx &) override {
        while (auto s = in_.recv()) out_.send(ruro::Scalar{s->value * k_});
    }
    bool set_param(const std::string &name, const ruro::Value &v) override {
        if (name != "k") return false;
        k_ = v.f;
        return true;
    }

private:
    ruro::Input<ruro::Scalar> in_;
    ruro::Output<ruro::Scalar> out_;
    double k_;
};

class Invert : public ruro::Node {
public:
    static ruro::Spec spec() {
        return ruro::Spec("cpptest/invert")
            .input<ruro::Image>("image")
            .output<ruro::Image>("inverted");
    }
    explicit Invert(ruro::Builder &b)
        : in_(b.input<ruro::Image>("image")), out_(b.output<ruro::Image>("inverted")) {}
    void step(ruro::Ctx &) override {
        while (auto s = in_.recv()) {
            buf_.assign(s->data.ptr, s->data.ptr + s->data.len);
            for (auto &b : buf_) b = static_cast<uint8_t>(255 - b);
            ruro::Image out = *s;
            out.data = ruro::view(buf_);
            out_.send(out);
        }
    }

private:
    ruro::Input<ruro::Image> in_;
    ruro::Output<ruro::Image> out_;
    std::vector<uint8_t> buf_;
};

class Thrower : public ruro::Node {
public:
    static ruro::Spec spec() {
        return ruro::Spec("cpptest/thrower")
            .output<ruro::Text>("out")
            .param("in_constructor", false)
            .param("name", "thrower");
    }
    explicit Thrower(ruro::Builder &b) : out_(b.output<ruro::Text>("out")) {
        if (b.param<bool>("in_constructor")) throw std::runtime_error("thrown in the constructor");
        name_ = b.param<std::string>("name");
    }
    void step(ruro::Ctx &ctx) override {
        if (ctx.steps() == 1) throw std::logic_error("thrown in step " + std::to_string(ctx.steps()));
        if (ctx.steps() == 2) throw 42;
        std::string msg = name_ + " step " + std::to_string(ctx.steps());
        out_.send(ruro::Text{ruro::str(msg)});
    }

private:
    ruro::Output<ruro::Text> out_;
    std::string name_;
};

// Asks for an input of the wrong type: refused before the host is asked.
class Mistyped : public ruro::Node {
public:
    static ruro::Spec spec() { return ruro::Spec("cpptest/mistyped").input<ruro::Image>("image"); }
    explicit Mistyped(ruro::Builder &b) : in_(b.input<ruro::Scalar>("image")) {}
    void step(ruro::Ctx &) override {}

private:
    ruro::Input<ruro::Scalar> in_;
};

RURO_PLUGIN(Gain, Invert, Thrower, Mistyped)
