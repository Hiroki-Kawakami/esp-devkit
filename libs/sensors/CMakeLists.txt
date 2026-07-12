# Reusable external-sensor drivers, grouped by vendor protocol layer on top of
# the vendor-neutral common/ helpers. The drivers build for both targets — on
# the simulator they run against the *_sim.c chip emulators attached to the
# idf_compat virtual I2C bus.
set(SENSORS_SRCS
    "common/sensor_i2c.c"
    "common/crc8.c"
    "sensirion/sensirion_i2c.c"
    "sensirion/sen55/sen55.c"
    "sensirion/scd40/scd40.c")

if(ESP_PLATFORM)
    idf_component_register(
        SRCS ${SENSORS_SRCS}
        INCLUDE_DIRS "sensirion/sen55" "sensirion/scd40"
        PRIV_INCLUDE_DIRS "common" "sensirion"
        # esp_driver_i2c is public: the driver headers expose the bus handle type.
        REQUIRES esp_driver_i2c
        PRIV_REQUIRES freertos)
else()
    idf_component_register(
        SRCS ${SENSORS_SRCS} "sensirion/sen55/sen55_sim.c" "sensirion/scd40/scd40_sim.c"
        INCLUDE_DIRS "sensirion/sen55" "sensirion/scd40"
        PRIV_INCLUDE_DIRS "common" "sensirion")
endif()
