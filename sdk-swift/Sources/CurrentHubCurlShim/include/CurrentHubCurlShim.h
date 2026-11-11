#ifndef CURRENT_HUB_CURL_SHIM_H
#define CURRENT_HUB_CURL_SHIM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct current_hub_curl_operation current_hub_curl_operation;

current_hub_curl_operation *current_hub_curl_operation_create(
  const char *url,
  const char *method,
  const uint8_t *request_body,
  size_t request_body_length,
  size_t maximum_response_bytes,
  long timeout_milliseconds
);

int current_hub_curl_operation_add_header(
  current_hub_curl_operation *operation,
  const char *header
);

int current_hub_curl_operation_set_expected_leaf_sha256(
  current_hub_curl_operation *operation,
  const char *expected_leaf_sha256
);

int current_hub_curl_operation_perform(current_hub_curl_operation *operation);
void current_hub_curl_operation_cancel(current_hub_curl_operation *operation);
void current_hub_curl_operation_destroy(current_hub_curl_operation *operation);

long current_hub_curl_operation_status_code(const current_hub_curl_operation *operation);
int current_hub_curl_operation_error_code(const current_hub_curl_operation *operation);
int current_hub_curl_operation_was_cancelled(const current_hub_curl_operation *operation);
int current_hub_curl_operation_response_too_large(const current_hub_curl_operation *operation);
int current_hub_curl_operation_headers_too_large(const current_hub_curl_operation *operation);
const uint8_t *current_hub_curl_operation_response_body(const current_hub_curl_operation *operation);
size_t current_hub_curl_operation_response_body_length(const current_hub_curl_operation *operation);
const uint8_t *current_hub_curl_operation_response_headers(const current_hub_curl_operation *operation);
size_t current_hub_curl_operation_response_headers_length(const current_hub_curl_operation *operation);
const char *current_hub_curl_operation_effective_url(const current_hub_curl_operation *operation);

#ifdef __cplusplus
}
#endif

#endif
