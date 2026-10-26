# The Windows 11 virtual camera; docs/building.md says how to build it
cmake_minimum_required(VERSION 3.20)
project(TelescopeCamera CXX)

if(NOT MSVC)
    message(FATAL_ERROR "The camera DLL builds with MSVC only")
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
# The C++ runtime goes into the DLL, so Frame Server doesn't need the VC++ redistributable to load it
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

add_library(TelescopeCamera SHARED
    dllmain.cpp
    media_source.cpp
    shared_frame.cpp
    TelescopeCamera.def
)
target_compile_definitions(TelescopeCamera PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX _WIN32_WINNT=0x0A00)
target_compile_options(TelescopeCamera PRIVATE /W4 /WX /permissive- /guard:cf)
# mfsensorgroup.dll is only loaded once a camera is set up, so the DLL itself still loads on older Windows
target_link_libraries(TelescopeCamera PRIVATE mfplat mfuuid mfsensorgroup advapi32 ole32 delayimp)
target_link_options(TelescopeCamera PRIVATE /DELAYLOAD:mfsensorgroup.dll /guard:cf /DYNAMICBASE /NXCOMPAT)
