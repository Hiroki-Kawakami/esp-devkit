/* Internal constants shared by idf_compat implementations. */
#pragma once

/* This namespace holds simulator hardware identity, analogous to data in
 * device eFuse. nvs_flash_erase() deliberately preserves it. */
#define IDF_COMPAT_SIM_NVS_NAMESPACE "idf_compat_sim"
