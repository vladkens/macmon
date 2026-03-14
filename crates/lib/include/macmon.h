#ifndef MACMON_H
#define MACMON_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum macmon_status_t {
  MACMON_STATUS_OK = 0,
  MACMON_STATUS_INVALID_ARGUMENT = 1,
  MACMON_STATUS_INIT_FAILED = 2,
  MACMON_STATUS_SAMPLE_FAILED = 3,
  MACMON_STATUS_PANIC = 4
} macmon_status_t;

typedef struct macmon_sampler macmon_sampler_t;

typedef struct macmon_usage_entry_t {
  const char *name;
  uint32_t freq_mhz;
  float usage;
  uint32_t units;
} macmon_usage_entry_t;

typedef struct macmon_usage_list_t {
  size_t len;
  macmon_usage_entry_t *ptr;
} macmon_usage_list_t;

typedef struct macmon_power_metrics_t {
  float cpu;
  float gpu;
  float ram;
  float sys;
  float gpu_ram;
  float ane;
  float all;
} macmon_power_metrics_t;

typedef struct macmon_mem_metrics_t {
  uint64_t ram_total;
  uint64_t ram_usage;
  uint64_t swap_total;
  uint64_t swap_usage;
} macmon_mem_metrics_t;

typedef struct macmon_temp_metrics_t {
  float cpu_temp_avg;
  float gpu_temp_avg;
} macmon_temp_metrics_t;

typedef struct macmon_metrics_t {
  macmon_usage_list_t cpu;
  macmon_usage_list_t gpu;
  macmon_power_metrics_t power;
  macmon_mem_metrics_t memory;
  macmon_temp_metrics_t temp;
} macmon_metrics_t;

typedef struct macmon_cpu_domain_t {
  /* Domain/channel prefix such as `ECPU` or `PCPU`. */
  const char *name;
  /* Number of CPU units (cores) in this domain. */
  uint32_t units;
  /* Length of `freqs_mhz`. */
  size_t freqs_len;
  /* Full DVFS frequency table for this domain in MHz, in pmgr order. */
  uint32_t *freqs_mhz;
} macmon_cpu_domain_t;

typedef struct macmon_soc_info_t {
  /* Machine model identifier reported by macOS, for example `Mac15,6`. */
  const char *mac_model;
  /* Marketing chip name reported by macOS, for example `Apple M3 Pro`. */
  const char *chip_name;
  /* Installed unified memory capacity in gigabytes. */
  uint8_t memory_gb;
  /* Sum of CPU units across all discovered CPU domains. */
  uint16_t cpu_cores_total;
  /* Length of `cpu_domains`. */
  size_t cpu_domains_len;
  /* CPU frequency domains discovered for this SoC. */
  macmon_cpu_domain_t *cpu_domains;
  /* GPU core count reported by macOS. */
  uint8_t gpu_cores;
  /* Length of `gpu_freqs_mhz`. */
  size_t gpu_freqs_len;
  /* Full GPU DVFS frequency table in MHz, in pmgr order. */
  uint32_t *gpu_freqs_mhz;
} macmon_soc_info_t;

macmon_status_t macmon_sampler_new(macmon_sampler_t **out_sampler);
void macmon_sampler_free(macmon_sampler_t *sampler);

macmon_status_t macmon_sampler_get_soc_info(
  macmon_sampler_t *sampler,
  macmon_soc_info_t *out_info
);
void macmon_soc_info_free(macmon_soc_info_t *info);

macmon_status_t macmon_sampler_get_metrics(
  macmon_sampler_t *sampler,
  uint32_t duration_ms,
  macmon_metrics_t *out_metrics
);
void macmon_metrics_free(macmon_metrics_t *metrics);

const char *macmon_last_error_message(void);

#ifdef __cplusplus
}
#endif

#endif
