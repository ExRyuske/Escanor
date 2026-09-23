cmake_minimum_required(VERSION 3.22.1)
project(escanor_audio C)

add_library(escanor_audio SHARED aaudio_capture.c)
target_link_libraries(escanor_audio aaudio)
