# lwip_netbench

The lwIP side of the devkit_net comparison on chips with their own Wi-Fi:
`libs/wifi` and the development firmware's `netbench.cpp` behind harness_rs,
measured with `firmware/esp_hosted_enhanced/development/netbench.py`. The
esp32s3 target runs on the AtomS3 Lite.

`wifi join <ssid> <password>` saves the network; later boots join it.
