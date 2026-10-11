/* macmon: rootless Apple Silicon metrics as JSON. https://github.com/vladkens/macmon */
#ifndef MACMON_H
#define MACMON_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Equals macmon_abi_version() and the "schema" key of every JSON document. Symbols and JSON keys
   are only added; removing, renaming or retyping one bumps it. */
#define MACMON_ABI_VERSION 1

typedef int32_t macmon_status_t;
enum {
  MACMON_STATUS_OK = 0,
  MACMON_STATUS_INVALID_ARGUMENT = 1, /* a NULL argument */
  MACMON_STATUS_INIT_FAILED = 2,      /* metric sources or SoC info unavailable */
  MACMON_STATUS_SAMPLE_FAILED = 3,
  MACMON_STATUS_PANIC = 4             /* internal error: the sampler is poisoned, only free it */
};

/* Functions returning macmon_status_t set *out to NULL first and, on an error,
   macmon_last_error_message(). A sampler may be used from any thread, one call at a time, never
   concurrently. */
typedef struct macmon_sampler macmon_sampler_t;

uint32_t macmon_abi_version(void);     /* MACMON_ABI_VERSION the library was built with */
const char *macmon_version(void);      /* macmon's version, "0.10.0"; static */

/* Primes the baseline, so the first sample covers a real interval. */
macmon_status_t macmon_sampler_new(macmon_sampler_t **out_sampler);
void macmon_sampler_free(macmon_sampler_t *sampler);  /* NULL: no-op */

/* Blocks until interval_ms after the previous sample; 0 returns at once with the interval since
   the previous call (or macmon_sampler_new). *out_json: a Metrics object with "schema" and
   "timestamp_ms" (Unix milliseconds). */
macmon_status_t macmon_sampler_next_json(macmon_sampler_t *sampler, uint32_t interval_ms,
                                         char **out_json);
macmon_status_t macmon_get_soc_info_json(char **out_json);  /* a SocInfo object with "schema" */
void macmon_string_free(char *json);                        /* NULL: no-op */

/* This thread's last error, NULL after a success; valid until the next call on this thread. */
const char *macmon_last_error_message(void);

#ifdef __cplusplus
}
#endif

#endif /* MACMON_H */
