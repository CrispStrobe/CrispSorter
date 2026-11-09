// Executed native mobile smoke: public photo C ABI plus real DBNet/Tesseract
// inference. Synthetic text only; not a camera, corpus, or performance claim.
#include "crispembed.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <iterator>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

static void require(bool ok, const char * message) {
    if (!ok) throw std::runtime_error(message);
}

static std::string quote(const std::string & text) {
    std::ostringstream out;
    out << '"';
    for (unsigned char c : text) {
        if (c == '"' || c == '\\') out << '\\' << c;
        else if (c < 32) out << "\\u00" << std::hex << std::setw(2)
                             << std::setfill('0') << int(c) << std::dec;
        else out << c;
    }
    return out.str() + '"';
}

static std::string normalized(const std::string & text) {
    std::string out;
    for (unsigned char c : text) {
        if (c >= 'a' && c <= 'z') out += char(c - 'a' + 'A');
        else if ((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')) out += char(c);
    }
    return out;
}

static double error_rate(const std::string & text) {
    const auto expected = normalized("CRISP MOBILE OCR Document 2048 Total 125.00");
    const auto actual = normalized(text);
    std::vector<size_t> row(actual.size() + 1);
    for (size_t j = 0; j < row.size(); ++j) row[j] = j;
    for (size_t i = 1; i <= expected.size(); ++i) {
        size_t previous = row[0];
        row[0] = i;
        for (size_t j = 1; j <= actual.size(); ++j) {
            size_t old = row[j];
            row[j] = std::min({row[j] + 1, row[j - 1] + 1,
                              previous + (expected[i - 1] != actual[j - 1])});
            previous = old;
        }
    }
    return double(row.back()) / expected.size();
}

using photo_buffer = std::unique_ptr<uint8_t, decltype(&crispembed_scan_photo_free_image)>;
using pipeline_handle = std::unique_ptr<void, decltype(&crispembed_ocr_pipeline_free)>;

static void write_ppm(const std::string & path, const uint8_t * rgb, int width, int height) {
    std::ofstream out(path, std::ios::binary);
    out << "P6\n" << width << ' ' << height << "\n255\n";
    out.write(reinterpret_cast<const char *>(rgb), size_t(width) * height * 3);
    require(bool(out), "Cannot write corrected synthetic page");
}

struct recognition {
    std::string text;
    double normalized_cer;
    int regions;
    double milliseconds;
};

static recognition recognize(void * context, const std::string & path) {
    int count = 0;
    const char * full_text = nullptr;
    float confidence = 0;
    auto start = std::chrono::steady_clock::now();
    auto regions = crispembed_ocr_pipeline_run(context, path.c_str(), &count, &full_text, &confidence);
    const double ms = std::chrono::duration<double, std::milli>(
        std::chrono::steady_clock::now() - start).count();
    require(regions && count > 0 && full_text, "Mobile OCR produced no regions");
    recognition result{full_text, 0, count, ms};
    result.normalized_cer = error_rate(result.text);
    // Preserve negative evidence before any assertion aborts this case.
    std::cerr << "{\"event\":\"native_ocr_observation\",\"path\":" << quote(path)
              << ",\"text\":" << quote(result.text) << ",\"normalized_cer\":"
              << result.normalized_cer << ",\"regions\":" << count
              << ",\"milliseconds\":" << ms << "}\n";
    require(result.text.find("2048") != std::string::npos, "Mobile OCR lost the synthetic document number");
    require(result.normalized_cer <= 0.25, "Mobile OCR exceeded the synthetic smoke error threshold");
    int review_length = 0;
    const char * review = crispembed_ocr_pipeline_review_json(context, &review_length);
    require(review && review_length > 0, "Native review snapshot unavailable");
    return result;
}

static std::string recognition_json(const recognition & r) {
    std::ostringstream out;
    out << "{\"text\":" << quote(r.text) << ",\"normalized_cer\":" << r.normalized_cer
        << ",\"regions\":" << r.regions << ",\"milliseconds\":" << r.milliseconds << '}';
    return out.str();
}

int main(int argc, char ** argv) {
    if (argc != 3) {
        std::cerr << "Usage: ocr-mobile-probe INPUT_DIRECTORY RESULT_JSON\n";
        return 2;
    }
    const std::string input = argv[1], receipt = argv[2];
    try {
        // Blank-image controls exercise the small-width no-votes boundary.
        for (int width : {3, 90, 100, 160}) {
            for (uint8_t fill : {0, 127, 255}) {
                std::vector<uint8_t> blank(size_t(width) * 120 * 3, fill);
                float correction = 99;
                require(crispembed_scan_photo_angle(blank.data(), blank.size(), width, 120, 3, 15,
                                                    &correction) == 0 && correction == 0,
                        "Blank mobile image invented a tilt correction");
            }
        }
        constexpr int width = 1400, height = 480;
        std::ifstream source(input + "/page.rgb", std::ios::binary);
        require(bool(source), "Synthetic RGB fixture is missing");
        std::vector<uint8_t> pixels((std::istreambuf_iterator<char>(source)), {});
        require(pixels.size() == size_t(width) * height * 3, "Synthetic RGB fixture has wrong dimensions");

        uint8_t * output = nullptr;
        int out_width = 0, out_height = 0;
        const float corners[8] = {0, 0, width - 1, 0, width - 1, height - 1, 0, height - 1};
        require(crispembed_scan_photo_perspective(pixels.data(), pixels.size(), width, height, 3,
                                                  corners, &output, &out_width, &out_height) == 0,
                "Mobile perspective call failed");
        {
            photo_buffer identity(output, crispembed_scan_photo_free_image);
            require(out_width == width && out_height == height &&
                    std::equal(pixels.begin(), pixels.end(), identity.get()),
                    "Full-frame mobile perspective changed the pixels");
        }
        require(crispembed_scan_photo_rotate(pixels.data(), pixels.size(), width, height, 3, 5,
                                             &output, &out_width, &out_height) == 0,
                "Mobile rotation call failed");
        photo_buffer tilted(output, crispembed_scan_photo_free_image);
        float correction = 0;
        require(crispembed_scan_photo_angle(tilted.get(), size_t(out_width) * out_height * 3,
                                            out_width, out_height, 3, 15, &correction) == 0,
                "Mobile angle analysis failed");
        std::cerr << "Known clockwise rotation: 5; measured correction: " << correction << '\n';
        require(std::isfinite(correction) && std::abs(correction + 5) <= 1.0,
                "Mobile tilt estimate missed the known clockwise rotation");
        int fixed_width = 0, fixed_height = 0;
        require(crispembed_scan_photo_rotate(tilted.get(), size_t(out_width) * out_height * 3,
                                             out_width, out_height, 3, correction,
                                             &output, &fixed_width, &fixed_height) == 0,
                "Mobile tilt correction failed");
        photo_buffer corrected(output, crispembed_scan_photo_free_image);
        const std::string corrected_path = input + "/corrected.ppm";
        write_ppm(corrected_path, corrected.get(), fixed_width, fixed_height);

        const std::string detector = input + "/dbnet-ic15-q8_0.gguf";
        const std::string recognizer = input + "/tesseract-eng-q8_0.gguf";
        // Exercise the model detector instead of the default segmentation router.
        // This affects this disposable probe process only, never the app's defaults.
        require(setenv("CRISPEMBED_TESSERACT_SEG_ROUTER", "0", 1) == 0,
                "Cannot select the probe's explicit detector path");
        crispembed_ocr_stage stage{};
        stage.source_type = 0;
        stage.engine = 6; // Public ABI: native Tesseract with DBNet detection.
        stage.model_a = detector.c_str();
        stage.model_b = recognizer.c_str();
        stage.det_prob_threshold = 0.3f;
        stage.det_box_threshold = 0.5f;
        stage.det_target_short = 768;
        stage.min_chars = 8;
        pipeline_handle pipeline(crispembed_ocr_pipeline_init_stages(
            0, nullptr, nullptr, nullptr, nullptr, nullptr, nullptr, &stage, 1, 2),
            crispembed_ocr_pipeline_free);
        require(bool(pipeline), "Mobile native OCR pipeline could not load verified model fixtures");
        const auto clean = recognize(pipeline.get(), input + "/page.ppm");
        const auto prepared = recognize(pipeline.get(), corrected_path);
        std::ofstream out(receipt);
        out << "{\"schema_version\":1,\"status\":\"passed\",\"synthetic_only\":true,"
            << "\"scope\":\"Executed native photo API and baseline OCR; not physical-camera or corpus qualification\","
            << "\"segmentation_router\":\"disabled_for_probe\",\"known_clockwise_degrees\":5,\"correction_clockwise_degrees\":" << correction
            << ",\"clean\":" << recognition_json(clean)
            << ",\"prepared\":" << recognition_json(prepared) << "}\n";
        require(bool(out), "Cannot write native probe receipt");
        return 0;
    } catch (const std::exception & error) {
        std::ofstream out(receipt);
        out << "{\"schema_version\":1,\"status\":\"failed\",\"synthetic_only\":true,\"error\":"
            << quote(error.what()) << "}\n";
        std::cerr << error.what() << '\n';
        return 1;
    }
}
