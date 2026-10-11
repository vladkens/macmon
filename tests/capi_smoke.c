/* Smoke test of libmacmon.dylib through include/macmon.h: `make capi-test`.
   MACMON_CAPI_ALLOW_NO_HW=1 (CI VMs without the metric sources) accepts INIT_FAILED and
   SAMPLE_FAILED with a message; without it any error fails, so real Apple Silicon fails loudly. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "macmon.h"

static int allow_no_hw;

#define CHECK(cond, ...)                                       \
  do {                                                         \
    if (!(cond)) {                                             \
      fprintf(stderr, "capi_smoke:%d: FAIL: ", __LINE__);      \
      fprintf(stderr, __VA_ARGS__);                            \
      fprintf(stderr, " (last error: %s)\n", last_error());    \
      exit(1);                                                 \
    }                                                          \
  } while (0)

static const char *last_error(void) {
  const char *msg = macmon_last_error_message();
  return msg ? msg : "NULL";
}

static int has_message(void) {
  const char *msg = macmon_last_error_message();
  return msg && *msg;
}

/* OK -> 1; an accepted missing-hardware error -> 0; anything else fails. */
static int ok_or_no_hw(macmon_status_t rs, macmon_status_t no_hw, const char *what) {
  if (rs == MACMON_STATUS_OK) {
    CHECK(macmon_last_error_message() == NULL, "%s: error message left after OK", what);
    return 1;
  }
  CHECK(allow_no_hw && rs == no_hw && has_message(), "%s returned %d", what, rs);
  printf("%s: skipped, no hardware (%d: %s)\n", what, rs, last_error());
  return 0;
}

/* A JSON object with "schema":1 and, for a sample, "timestamp_ms"; no deprecated keys. */
static unsigned long long check_json(const char *json, int sample, const char *what) {
  size_t len = json ? strlen(json) : 0;
  CHECK(len > 2 && json[0] == '{' && json[len - 1] == '}', "%s: not a JSON object", what);
  CHECK(strstr(json, "\"schema\":1") != NULL, "%s: no \"schema\":1", what);
  CHECK(strstr(json, "\"ecpu_") == NULL && strstr(json, "\"pcpu_") == NULL,
        "%s: deprecated keys", what);
  if (!sample) return 0;

  const char *ts = strstr(json, "\"timestamp_ms\":");
  CHECK(ts != NULL, "%s: no timestamp_ms", what);
  unsigned long long ms = strtoull(ts + strlen("\"timestamp_ms\":"), NULL, 10);
  CHECK(ms > 1600000000000ULL, "%s: timestamp_ms %llu", what, ms);
  return ms;
}

int main(void) {
  const char *env = getenv("MACMON_CAPI_ALLOW_NO_HW");
  allow_no_hw = env && strcmp(env, "1") == 0;

  const char *version = macmon_version();
  CHECK(version && *version, "empty version");
  CHECK(macmon_abi_version() == MACMON_ABI_VERSION, "ABI %u, header %d", macmon_abi_version(),
        MACMON_ABI_VERSION);
  printf("macmon_version: %s\nabi: %u\n", version, macmon_abi_version());

  /* NULL arguments: INVALID_ARGUMENT with a message, out set to NULL */
  char *json = (char *)1;
  CHECK(macmon_sampler_new(NULL) == MACMON_STATUS_INVALID_ARGUMENT && has_message(),
        "sampler_new(NULL)");
  CHECK(macmon_get_soc_info_json(NULL) == MACMON_STATUS_INVALID_ARGUMENT && has_message(),
        "get_soc_info_json(NULL)");
  CHECK(macmon_sampler_next_json(NULL, 0, &json) == MACMON_STATUS_INVALID_ARGUMENT &&
            has_message() && json == NULL,
        "sampler_next_json(NULL sampler)");
  CHECK(macmon_sampler_next_json(NULL, 0, NULL) == MACMON_STATUS_INVALID_ARGUMENT &&
            has_message(),
        "sampler_next_json(NULL out_json)");
  macmon_sampler_free(NULL);
  macmon_string_free(NULL);
  printf("null arguments: ok\n");

  if (ok_or_no_hw(macmon_get_soc_info_json(&json), MACMON_STATUS_INIT_FAILED, "soc info")) {
    check_json(json, 0, "soc info");
    const char *chip = strstr(json, "\"chip_name\":\"");
    CHECK(chip != NULL, "soc info: no chip_name");
    chip += strlen("\"chip_name\":\"");
    printf("soc info: %zu bytes, chip %.*s\n", strlen(json), (int)strcspn(chip, "\""), chip);
    macmon_string_free(json);
  }

  macmon_sampler_t *sampler = NULL;
  if (!ok_or_no_hw(macmon_sampler_new(&sampler), MACMON_STATUS_INIT_FAILED, "sampler_new")) {
    printf("capi smoke test: ok (no hardware)\n");
    return 0;
  }
  CHECK(sampler != NULL, "sampler_new: NULL sampler");

  /* a 100 ms interval, then 0 after sleeping 100 ms: returns at once with the elapsed interval */
  unsigned long long prev = 0;
  for (int i = 1; i <= 2; i++) {
    if (i == 2) usleep(100 * 1000);
    macmon_status_t rs = macmon_sampler_next_json(sampler, i == 1 ? 100 : 0, &json);
    if (!ok_or_no_hw(rs, MACMON_STATUS_SAMPLE_FAILED, "sample")) break;
    unsigned long long ms = check_json(json, 1, "sample");
    CHECK(ms >= prev, "sample %d: timestamp_ms went back", i);
    printf("sample %d: %zu bytes, timestamp_ms %llu\n", i, strlen(json), ms);
    macmon_string_free(json);
    prev = ms;
  }

  macmon_sampler_free(sampler);
  printf("capi smoke test: ok\n");
  return 0;
}
