// A ruro node in C++: turns images into black and white (std/Image to a
// mono8 std/Image), with a live-tunable threshold.
#include <ruro/ruro.hpp>

#include <vector>

class Threshold : public ruro::Node {
public:
    static ruro::Spec spec() {
        return ruro::Spec("example/cpp-threshold")
            .doc("Black-and-white images, in C++.")
            .input<ruro::Image>("image")
            .output<ruro::Image>("mask")
            .param("level", int64_t{128}, 0.0, 255.0, "brightness above which a pixel is white");
    }

    explicit Threshold(ruro::Builder &b)
        : in_(b.input<ruro::Image>("image")),
          out_(b.output<ruro::Image>("mask")),
          level_(b.param<int>("level")) {}

    void step(ruro::Ctx &) override {
        // Only the newest image matters.
        auto img = in_.latest();
        if (!img) return;
        const size_t channels = img->encoding == RURO_RGBA8 ? 4 : img->encoding == RURO_RGB8 ? 3 : 1;
        if (img->encoding == RURO_MONO16) throw std::invalid_argument("mono16 images are not supported");
        const size_t pixels = size_t{img->width} * img->height;
        mask_.resize(pixels);
        for (size_t i = 0; i < pixels; i++) {
            const uint8_t *p = img->data.ptr + i * channels;
            int brightness = channels == 1 ? p[0] : (p[0] * 299 + p[1] * 587 + p[2] * 114) / 1000;
            mask_[i] = brightness > level_ ? 255 : 0;
        }
        ruro::Image out{img->width, img->height, RURO_MONO8, ruro::view(mask_)};
        out_.send(out);
    }

    bool set_param(const std::string &name, const ruro::Value &v) override {
        if (name != "level") return false;
        level_ = static_cast<int>(v.i);
        return true;
    }

private:
    ruro::Input<ruro::Image> in_;
    ruro::Output<ruro::Image> out_;
    std::vector<uint8_t> mask_;
    int level_;
};

// Called from lowpass.c's entry point: one library, two node types. A
// library of C++ nodes only would end with RURO_PLUGIN(Threshold) instead.
extern "C" int example_register_threshold(const ruro_api *api, ruro_registrar *reg) {
    return ruro::detail::register_all<Threshold>(api, reg);
}
