/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#pragma once

#include <functional>
#include <memory>

#include "esp_err.h"

namespace usb_host {

class MscDevice;
class UacDevice;
class UacCaptureDevice;
class UvcDevice;

// Called on the usb_host worker task; blocking them stalls enumeration.
struct Callbacks {
    std::function<void(std::shared_ptr<MscDevice>)> msc_connected;
    std::function<void(const std::shared_ptr<MscDevice>&)> msc_disconnected;
    std::function<void(std::shared_ptr<UacDevice>)> uac_connected;
    std::function<void(const std::shared_ptr<UacDevice>&)> uac_disconnected;
    std::function<void(std::shared_ptr<UacCaptureDevice>)> uac_capture_connected;
    std::function<void(const std::shared_ptr<UacCaptureDevice>&)> uac_capture_disconnected;
    std::function<void(std::shared_ptr<UvcDevice>)> uvc_connected;
    std::function<void(const std::shared_ptr<UvcDevice>&)> uvc_disconnected;
};

// VBUS is not touched: power the port as the caller sees fit.
esp_err_t install(Callbacks callbacks);

}  // namespace usb_host
