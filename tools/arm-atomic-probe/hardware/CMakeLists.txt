# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.13)
include($ENV{PICO_SDK_PATH}/external/pico_sdk_import.cmake)

project(flintos_arm_atomic_hardware_probe C CXX ASM)
set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
pico_sdk_init()

add_executable(arm-atomic-hardware-probe main.c)
target_link_libraries(arm-atomic-hardware-probe pico_stdlib pico_multicore hardware_sync)
pico_enable_stdio_usb(arm-atomic-hardware-probe 1)
pico_enable_stdio_uart(arm-atomic-hardware-probe 0)
pico_add_extra_outputs(arm-atomic-hardware-probe)

