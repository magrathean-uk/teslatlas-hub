#include "CurrentHubCurlShim.h"

#include <curl/curl.h>
#include <openssl/crypto.h>
#include <openssl/sha.h>
#include <openssl/ssl.h>
#include <openssl/x509.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

#define CURRENT_HUB_MAXIMUM_HEADER_BYTES 65536

struct current_hub_curl_operation {
  char *url;
  char *method;
  uint8_t *request_body;
  size_t request_body_length;
  size_t maximum_response_bytes;
  long timeout_milliseconds;
  struct curl_slist *headers;
  uint8_t *response_body;
  size_t response_body_length;
  uint8_t *response_headers;
  size_t response_headers_length;
  char *effective_url;
  long status_code;
  int error_code;
  int response_too_large;
  int headers_too_large;
  char expected_leaf_sha256[65];
  int leaf_pin_verified;
  int leaf_pin_mismatch;
  int (*previous_verify_callback)(int, X509_STORE_CTX *);
  atomic_bool cancelled;
};

static pthread_once_t current_hub_curl_once = PTHREAD_ONCE_INIT;
static int current_hub_ssl_context_operation_index = -1;

static void current_hub_curl_initialize(void) {
  curl_global_init(CURL_GLOBAL_DEFAULT);
  current_hub_ssl_context_operation_index = SSL_CTX_get_ex_new_index(
    0,
    NULL,
    NULL,
    NULL,
    NULL
  );
}

static int current_hub_append(
  uint8_t **destination,
  size_t *destination_length,
  const uint8_t *source,
  size_t source_length,
  size_t maximum_length
) {
  if (*destination_length > maximum_length
      || source_length > maximum_length - *destination_length) {
    return 0;
  }
  if (source_length == 0) {
    return 1;
  }
  uint8_t *grown = realloc(*destination, *destination_length + source_length);
  if (grown == NULL) {
    return 0;
  }
  memcpy(grown + *destination_length, source, source_length);
  *destination = grown;
  *destination_length += source_length;
  return 1;
}

static size_t current_hub_curl_write_body(
  char *data,
  size_t size,
  size_t count,
  void *context
) {
  current_hub_curl_operation *operation = context;
  if (atomic_load_explicit(&operation->cancelled, memory_order_relaxed)) {
    return 0;
  }
  if (count != 0 && size > SIZE_MAX / count) {
    operation->response_too_large = 1;
    return 0;
  }
  size_t length = size * count;
  if (!current_hub_append(
        &operation->response_body,
        &operation->response_body_length,
        (const uint8_t *)data,
        length,
        operation->maximum_response_bytes
      )) {
    operation->response_too_large = 1;
    return 0;
  }
  return length;
}

static size_t current_hub_curl_write_header(
  char *data,
  size_t size,
  size_t count,
  void *context
) {
  current_hub_curl_operation *operation = context;
  if (atomic_load_explicit(&operation->cancelled, memory_order_relaxed)) {
    return 0;
  }
  if (count != 0 && size > SIZE_MAX / count) {
    operation->headers_too_large = 1;
    return 0;
  }
  size_t length = size * count;
  if (!current_hub_append(
        &operation->response_headers,
        &operation->response_headers_length,
        (const uint8_t *)data,
        length,
        CURRENT_HUB_MAXIMUM_HEADER_BYTES
      )) {
    operation->headers_too_large = 1;
    return 0;
  }
  return length;
}

static int current_hub_curl_progress(
  void *context,
  curl_off_t download_total,
  curl_off_t download_now,
  curl_off_t upload_total,
  curl_off_t upload_now
) {
  (void)download_total;
  (void)download_now;
  (void)upload_total;
  (void)upload_now;
  current_hub_curl_operation *operation = context;
  return atomic_load_explicit(&operation->cancelled, memory_order_relaxed) ? 1 : 0;
}

static int current_hub_curl_verify_certificate(
  int preverify_ok,
  X509_STORE_CTX *store_context
) {
  SSL *ssl = X509_STORE_CTX_get_ex_data(
    store_context,
    SSL_get_ex_data_X509_STORE_CTX_idx()
  );
  if (ssl == NULL) {
    return 0;
  }
  SSL_CTX *ssl_context = SSL_get_SSL_CTX(ssl);
  current_hub_curl_operation *operation = SSL_CTX_get_ex_data(
    ssl_context,
    current_hub_ssl_context_operation_index
  );
  if (operation == NULL) {
    return 0;
  }
  int accepted = operation->previous_verify_callback == NULL
    ? preverify_ok
    : operation->previous_verify_callback(preverify_ok, store_context);
  if (!accepted) {
    return 0;
  }
  if (X509_STORE_CTX_get_error_depth(store_context) != 0) {
    return 1;
  }
  X509 *leaf = X509_STORE_CTX_get_current_cert(store_context);
  if (leaf == NULL) {
    return 0;
  }

  int der_length = i2d_X509(leaf, NULL);
  if (der_length <= 0) {
    operation->leaf_pin_mismatch = 1;
    return 0;
  }
  uint8_t *der = malloc((size_t)der_length);
  if (der == NULL) {
    operation->leaf_pin_mismatch = 1;
    return 0;
  }
  unsigned char *cursor = der;
  if (i2d_X509(leaf, &cursor) != der_length) {
    free(der);
    operation->leaf_pin_mismatch = 1;
    return 0;
  }
  unsigned char digest[SHA256_DIGEST_LENGTH];
  SHA256(der, (size_t)der_length, digest);
  free(der);

  static const char hexadecimal[] = "0123456789abcdef";
  char actual[65];
  for (size_t index = 0; index < SHA256_DIGEST_LENGTH; index++) {
    actual[index * 2] = hexadecimal[digest[index] >> 4];
    actual[index * 2 + 1] = hexadecimal[digest[index] & 0x0f];
  }
  actual[64] = '\0';
  if (CRYPTO_memcmp(actual, operation->expected_leaf_sha256, 64) != 0) {
    operation->leaf_pin_mismatch = 1;
    return 0;
  }
  operation->leaf_pin_verified = 1;
  return 1;
}

static CURLcode current_hub_curl_configure_ssl_context(
  CURL *curl,
  void *ssl_context,
  void *context
) {
  (void)curl;
  current_hub_curl_operation *operation = context;
  SSL_CTX *openssl_context = ssl_context;
  if (operation == NULL || openssl_context == NULL) {
    return CURLE_SSL_CERTPROBLEM;
  }
  if (current_hub_ssl_context_operation_index < 0
      || SSL_CTX_set_ex_data(
        openssl_context,
        current_hub_ssl_context_operation_index,
        operation
      ) == 0) {
    return CURLE_SSL_CERTPROBLEM;
  }
  operation->previous_verify_callback = SSL_CTX_get_verify_callback(openssl_context);
  SSL_CTX_set_verify(
    openssl_context,
    SSL_CTX_get_verify_mode(openssl_context) | SSL_VERIFY_PEER,
    current_hub_curl_verify_certificate
  );
  return CURLE_OK;
}

current_hub_curl_operation *current_hub_curl_operation_create(
  const char *url,
  const char *method,
  const uint8_t *request_body,
  size_t request_body_length,
  size_t maximum_response_bytes,
  long timeout_milliseconds
) {
  if (url == NULL || method == NULL || maximum_response_bytes == 0
      || timeout_milliseconds <= 0) {
    return NULL;
  }
  current_hub_curl_operation *operation = calloc(1, sizeof(*operation));
  if (operation == NULL) {
    return NULL;
  }
  operation->url = strdup(url);
  operation->method = strdup(method);
  operation->maximum_response_bytes = maximum_response_bytes;
  operation->timeout_milliseconds = timeout_milliseconds;
  atomic_init(&operation->cancelled, 0);
  if (request_body_length > 0) {
    operation->request_body = malloc(request_body_length);
    if (operation->request_body != NULL) {
      memcpy(operation->request_body, request_body, request_body_length);
      operation->request_body_length = request_body_length;
    }
  }
  if (operation->url == NULL || operation->method == NULL
      || (request_body_length > 0 && operation->request_body == NULL)) {
    current_hub_curl_operation_destroy(operation);
    return NULL;
  }
  return operation;
}

int current_hub_curl_operation_add_header(
  current_hub_curl_operation *operation,
  const char *header
) {
  if (operation == NULL || header == NULL) {
    return 0;
  }
  struct curl_slist *updated = curl_slist_append(operation->headers, header);
  if (updated == NULL) {
    return 0;
  }
  operation->headers = updated;
  return 1;
}

int current_hub_curl_operation_set_expected_leaf_sha256(
  current_hub_curl_operation *operation,
  const char *expected_leaf_sha256
) {
  if (operation == NULL || expected_leaf_sha256 == NULL
      || strlen(expected_leaf_sha256) != 64) {
    return 0;
  }
  for (size_t index = 0; index < 64; index++) {
    char value = expected_leaf_sha256[index];
    if (!((value >= '0' && value <= '9') || (value >= 'a' && value <= 'f'))) {
      return 0;
    }
  }
  memcpy(operation->expected_leaf_sha256, expected_leaf_sha256, 65);
  return 1;
}

int current_hub_curl_operation_perform(current_hub_curl_operation *operation) {
  if (operation == NULL) {
    return CURLE_FAILED_INIT;
  }
  pthread_once(&current_hub_curl_once, current_hub_curl_initialize);
  CURL *curl = curl_easy_init();
  if (curl == NULL) {
    operation->error_code = CURLE_FAILED_INIT;
    return operation->error_code;
  }

  curl_easy_setopt(curl, CURLOPT_URL, operation->url);
  curl_easy_setopt(curl, CURLOPT_CUSTOMREQUEST, operation->method);
  curl_easy_setopt(curl, CURLOPT_HTTPHEADER, operation->headers);
  curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 0L);
  curl_easy_setopt(curl, CURLOPT_MAXREDIRS, 0L);
  curl_easy_setopt(curl, CURLOPT_PROTOCOLS, (long)(CURLPROTO_HTTP | CURLPROTO_HTTPS));
  curl_easy_setopt(curl, CURLOPT_REDIR_PROTOCOLS, (long)(CURLPROTO_HTTP | CURLPROTO_HTTPS));
  curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);
  curl_easy_setopt(curl, CURLOPT_SSL_VERIFYPEER, 1L);
  curl_easy_setopt(curl, CURLOPT_SSL_VERIFYHOST, 2L);
  const char *certificate_file = getenv("SSL_CERT_FILE");
  if (certificate_file != NULL && certificate_file[0] != '\0') {
    curl_easy_setopt(curl, CURLOPT_CAINFO, certificate_file);
  }
  const char *certificate_directory = getenv("SSL_CERT_DIR");
  if (certificate_directory != NULL && certificate_directory[0] != '\0') {
    curl_easy_setopt(curl, CURLOPT_CAPATH, certificate_directory);
  }
  long connect_timeout = operation->timeout_milliseconds < 20000L
    ? operation->timeout_milliseconds : 20000L;
  curl_easy_setopt(curl, CURLOPT_CONNECTTIMEOUT_MS, connect_timeout);
  curl_easy_setopt(curl, CURLOPT_TIMEOUT_MS, operation->timeout_milliseconds);
  curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, current_hub_curl_write_body);
  curl_easy_setopt(curl, CURLOPT_WRITEDATA, operation);
  curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, current_hub_curl_write_header);
  curl_easy_setopt(curl, CURLOPT_HEADERDATA, operation);
  curl_easy_setopt(curl, CURLOPT_NOPROGRESS, 0L);
  curl_easy_setopt(curl, CURLOPT_XFERINFOFUNCTION, current_hub_curl_progress);
  curl_easy_setopt(curl, CURLOPT_XFERINFODATA, operation);
  if (operation->expected_leaf_sha256[0] != '\0') {
    const curl_version_info_data *version = curl_version_info(CURLVERSION_NOW);
    if (version == NULL || version->ssl_version == NULL
        || strncmp(version->ssl_version, "OpenSSL/", 8) != 0) {
      operation->error_code = CURLE_NOT_BUILT_IN;
      curl_easy_cleanup(curl);
      return operation->error_code;
    }
    curl_easy_setopt(curl, CURLOPT_SSL_CTX_FUNCTION, current_hub_curl_configure_ssl_context);
    curl_easy_setopt(curl, CURLOPT_SSL_CTX_DATA, operation);
  }
  if (operation->request_body != NULL || strcmp(operation->method, "POST") == 0) {
    curl_easy_setopt(curl, CURLOPT_POSTFIELDS, operation->request_body);
    curl_easy_setopt(
      curl,
      CURLOPT_POSTFIELDSIZE_LARGE,
      (curl_off_t)operation->request_body_length
    );
  }

  CURLcode result = curl_easy_perform(curl);
  if (result == CURLE_OK && operation->expected_leaf_sha256[0] != '\0'
      && !operation->leaf_pin_verified) {
    result = CURLE_PEER_FAILED_VERIFICATION;
  }
  operation->error_code = (int)result;
  curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &operation->status_code);
  char *effective_url = NULL;
  curl_easy_getinfo(curl, CURLINFO_EFFECTIVE_URL, &effective_url);
  if (effective_url != NULL) {
    operation->effective_url = strdup(effective_url);
  }
  curl_easy_cleanup(curl);
  return operation->error_code;
}

void current_hub_curl_operation_cancel(current_hub_curl_operation *operation) {
  if (operation != NULL) {
    atomic_store_explicit(&operation->cancelled, 1, memory_order_relaxed);
  }
}

void current_hub_curl_operation_destroy(current_hub_curl_operation *operation) {
  if (operation == NULL) {
    return;
  }
  curl_slist_free_all(operation->headers);
  free(operation->url);
  free(operation->method);
  free(operation->request_body);
  free(operation->response_body);
  free(operation->response_headers);
  free(operation->effective_url);
  free(operation);
}

long current_hub_curl_operation_status_code(const current_hub_curl_operation *operation) {
  return operation == NULL ? 0 : operation->status_code;
}

int current_hub_curl_operation_error_code(const current_hub_curl_operation *operation) {
  return operation == NULL ? CURLE_FAILED_INIT : operation->error_code;
}

int current_hub_curl_operation_was_cancelled(const current_hub_curl_operation *operation) {
  return operation != NULL
    && atomic_load_explicit(&operation->cancelled, memory_order_relaxed);
}

int current_hub_curl_operation_response_too_large(const current_hub_curl_operation *operation) {
  return operation != NULL && operation->response_too_large;
}

int current_hub_curl_operation_headers_too_large(const current_hub_curl_operation *operation) {
  return operation != NULL && operation->headers_too_large;
}

const uint8_t *current_hub_curl_operation_response_body(const current_hub_curl_operation *operation) {
  return operation == NULL ? NULL : operation->response_body;
}

size_t current_hub_curl_operation_response_body_length(const current_hub_curl_operation *operation) {
  return operation == NULL ? 0 : operation->response_body_length;
}

const uint8_t *current_hub_curl_operation_response_headers(const current_hub_curl_operation *operation) {
  return operation == NULL ? NULL : operation->response_headers;
}

size_t current_hub_curl_operation_response_headers_length(const current_hub_curl_operation *operation) {
  return operation == NULL ? 0 : operation->response_headers_length;
}

const char *current_hub_curl_operation_effective_url(const current_hub_curl_operation *operation) {
  return operation == NULL ? NULL : operation->effective_url;
}
