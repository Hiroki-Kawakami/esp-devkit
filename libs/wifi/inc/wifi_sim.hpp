/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <string>
#include <vector>

#include "wifi_manager.hpp"

namespace wifi::sim {

void set_aps(std::vector<AP> access_points);
void set_next_connect_result(Result result);
void set_event_delay_ms(int delay_ms);
void set_ip(std::string ip);
void drop_link();
void register_harness_commands();

}  // namespace wifi::sim
