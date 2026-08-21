# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.13)

include($ENV{PICO_SDK_PATH}/external/pico_sdk_import.cmake)

project(flintos_rp2040_first_light C CXX ASM)
set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)

pico_sdk_init()

add_executable(rp2040-first-light main.c)
target_link_libraries(rp2040-first-light pico_stdlib hardware_gpio hardware_uart)

# The UART marker is deliberately on physical pins, not USB. USB enumeration
# would add a second, unrelated variable to this boot/clock/GPIO probe.
pico_enable_stdio_usb(rp2040-first-light 0)
pico_enable_stdio_uart(rp2040-first-light 0)
pico_add_extra_outputs(rp2040-first-light)
