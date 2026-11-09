// Execute the exact API version used by the Rust binding against the target
// runtime. No external weights, host inference, or substitute output is used.
#include "onnxruntime_c_api.h"
#include "add_model.h"

#include <array>
#include <chrono>
#include <cmath>
#include <fstream>
#include <iostream>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>

static_assert(ORT_API_VERSION == 23, "The mobile Rust binding requires API 23");

static std::string quote(const std::string &value) {
    std::ostringstream result;
    result << '"';
    for (unsigned char ch : value) {
        if (ch == '"' || ch == '\\') result << '\\' << ch;
        else if (ch < 0x20) {
            static const char hex[] = "0123456789abcdef";
            result << "\\u00" << hex[ch >> 4] << hex[ch & 15];
        } else result << ch;
    }
    result << '"';
    return result.str();
}

int main(int argc, char **argv) {
    if (argc != 3) {
        std::cerr << "Usage: mobile-ort-probe EXPECTED_VERSION OUTPUT_JSON\n";
        return 2;
    }
    std::string version;
    const auto started = std::chrono::steady_clock::now();
    try {
        const OrtApiBase *base = OrtGetApiBase();
        if (!base) throw std::runtime_error("OrtGetApiBase returned null");
        const char *reported_version = base->GetVersionString();
        if (!reported_version) throw std::runtime_error("ONNX Runtime returned no version");
        version = reported_version;
        if (version != argv[1]) throw std::runtime_error("Loaded ONNX Runtime version differs from the pinned artifact");
        const OrtApi *api = base->GetApi(ORT_API_VERSION);
        if (!api) throw std::runtime_error("Loaded ONNX Runtime does not implement API 23");
        const auto check = [&](OrtStatus *status) {
            if (!status) return;
            const std::string error = api->GetErrorMessage(status);
            api->ReleaseStatus(status);
            throw std::runtime_error(error);
        };
        OrtEnv *env_raw = nullptr;
        check(api->CreateEnv(ORT_LOGGING_LEVEL_WARNING, "crispsorter-mobile-runtime", &env_raw));
        std::unique_ptr<OrtEnv, decltype(api->ReleaseEnv)> env(env_raw, api->ReleaseEnv);
        OrtSessionOptions *options_raw = nullptr;
        check(api->CreateSessionOptions(&options_raw));
        std::unique_ptr<OrtSessionOptions, decltype(api->ReleaseSessionOptions)> options(options_raw, api->ReleaseSessionOptions);
        check(api->SetIntraOpNumThreads(options.get(), 1));
        check(api->SetInterOpNumThreads(options.get(), 1));
        check(api->SetSessionGraphOptimizationLevel(options.get(), ORT_DISABLE_ALL));
        OrtSession *session_raw = nullptr;
        check(api->CreateSessionFromArray(env.get(), ADD_MODEL, sizeof(ADD_MODEL), options.get(), &session_raw));
        std::unique_ptr<OrtSession, decltype(api->ReleaseSession)> session(session_raw, api->ReleaseSession);
        OrtMemoryInfo *memory_raw = nullptr;
        check(api->CreateCpuMemoryInfo(OrtArenaAllocator, OrtMemTypeDefault, &memory_raw));
        std::unique_ptr<OrtMemoryInfo, decltype(api->ReleaseMemoryInfo)> memory(memory_raw, api->ReleaseMemoryInfo);
        std::array<float, 4> a{1, 2, 3, 4}, b{10, 20, 30, 40}, expected{11, 22, 33, 44};
        const std::array<int64_t, 2> shape{2, 2};
        OrtValue *a_raw = nullptr, *b_raw = nullptr;
        check(api->CreateTensorWithDataAsOrtValue(memory.get(), a.data(), sizeof(a), shape.data(), shape.size(), ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT, &a_raw));
        std::unique_ptr<OrtValue, decltype(api->ReleaseValue)> left(a_raw, api->ReleaseValue);
        check(api->CreateTensorWithDataAsOrtValue(memory.get(), b.data(), sizeof(b), shape.data(), shape.size(), ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT, &b_raw));
        std::unique_ptr<OrtValue, decltype(api->ReleaseValue)> right(b_raw, api->ReleaseValue);
        const char *names[] = {"A", "B"}, *outputs[] = {"Y"};
        const OrtValue *inputs[] = {left.get(), right.get()};
        for (int iteration = 0; iteration < 3; iteration++) {
            OrtValue *result_raw = nullptr;
            check(api->Run(session.get(), nullptr, names, inputs, 2, outputs, 1, &result_raw));
            std::unique_ptr<OrtValue, decltype(api->ReleaseValue)> result(result_raw, api->ReleaseValue);
            OrtTensorTypeAndShapeInfo *shape_raw = nullptr;
            check(api->GetTensorTypeAndShape(result.get(), &shape_raw));
            std::unique_ptr<OrtTensorTypeAndShapeInfo, decltype(api->ReleaseTensorTypeAndShapeInfo)> info(shape_raw, api->ReleaseTensorTypeAndShapeInfo);
            size_t count = 0, dimensions = 0;
            ONNXTensorElementDataType type;
            check(api->GetTensorShapeElementCount(info.get(), &count));
            check(api->GetDimensionsCount(info.get(), &dimensions));
            check(api->GetTensorElementType(info.get(), &type));
            if (count != 4 || dimensions != 2 || type != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT) throw std::runtime_error("Unexpected Add output type/shape");
            std::array<int64_t, 2> actual_shape{};
            check(api->GetDimensions(info.get(), actual_shape.data(), actual_shape.size()));
            if (actual_shape != shape) throw std::runtime_error("Add output dimensions changed");
            void *raw = nullptr;
            check(api->GetTensorMutableData(result.get(), &raw));
            if (!raw) throw std::runtime_error("Add returned no tensor data");
            const auto *actual = static_cast<const float *>(raw);
            for (size_t i = 0; i < expected.size(); i++) {
                if (!std::isfinite(actual[i]) || actual[i] != expected[i]) throw std::runtime_error("Actual target Add inference returned incorrect values");
            }
        }
        const auto elapsed = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
        std::ofstream file(argv[2]);
        file << "{\"schema_version\":1,\"status\":\"passed\",\"runtime_version\":" << quote(version)
             << ",\"api_version\":23,\"model_bytes\":" << sizeof(ADD_MODEL)
             << ",\"iterations\":3,\"output_shape\":[2,2],\"output\":[11,22,33,44],\"elapsed_ms\":" << elapsed << "}\n";
        if (!file) throw std::runtime_error("Could not save actual inference evidence");
        std::cout << "Executed ONNX Runtime " << version << " API 23: Add [2,2] -> [11,22,33,44], three iterations\n";
        return 0;
    } catch (const std::exception &error) {
        std::ofstream file(argv[2]);
        file << "{\"schema_version\":1,\"status\":\"failed\",\"runtime_version\":" << quote(version)
             << ",\"api_version\":23,\"error\":" << quote(error.what()) << "}\n";
        std::cerr << error.what() << '\n';
        return 1;
    }
}
