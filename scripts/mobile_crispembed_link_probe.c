/* Force the linker to resolve the OCR API and its native dependencies.
 * This is a link probe only; it does not execute OCR or load a model. */
#include "crispembed.h"

int main(void) {
    __typeof__(&crispembed_ocr_model_init) volatile init = &crispembed_ocr_model_init;
    __typeof__(&crispembed_ocr_model_recognize) volatile recognize = &crispembed_ocr_model_recognize;
    __typeof__(&crispembed_ocr_model_free) volatile release = &crispembed_ocr_model_free;
    __typeof__(&crispembed_ocr_pipeline_init) volatile pipeline = &crispembed_ocr_pipeline_init;
    __typeof__(&crispembed_ocr_pipeline_run) volatile run = &crispembed_ocr_pipeline_run;
    __typeof__(&crispembed_ocr_pipeline_free) volatile end = &crispembed_ocr_pipeline_free;
    return !init || !recognize || !release || !pipeline || !run || !end;
}
