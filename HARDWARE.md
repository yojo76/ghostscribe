# GhostScribe server host

Reference hardware for the GhostScribe development / reference server.
Kept in the repo so capacity, driver, and compatibility decisions have a
known baseline.

## System
- **Distro:** Linux Mint 22.3 "Zena" (Ubuntu 24.04 "noble" base)
- **Kernel:** 6.17.0-20-generic, x86_64
- **Desktop:** Cinnamon 6.6.7 on X11 (X.Org 21.1.11 + Xwayland 23.2.6)
- **Display manager:** LightDM 1.30.0
- **Init:** systemd 255, default target `graphical`

## Motherboard / firmware
- **Board:** ASRock A520M-HDV
- **UEFI:** American Megatrends P3.90, 2025-10-01

## CPU
- **Model:** AMD Ryzen 5 5500GT (Zen 3, Cezanne APU)
- **Cores / threads:** 6C / 12T, SMT enabled

## GPU
- **Primary (inference):** NVIDIA (MSI), driver `nvidia` 580.126.09, PCIe 8 GT/s x8
- **Secondary (iGPU):** AMD Cezanne Radeon Vega (GCN 5), `amdgpu` kernel driver, PCIe x16
- **X server:** loads both `amdgpu` and `nvidia`; `radeonsi` DRI for the iGPU

> The NVIDIA card is the intended target for `faster-whisper`. The iGPU
> can drive the display so the dGPU stays dedicated to inference. The
> product spec targets an RTX 5060 Ti (Blackwell, sm_120); confirm the
> installed card with `nvidia-smi` before choosing CUDA / ctranslate2
> versions — Blackwell needs CUDA 12.8+ and `ctranslate2` built against it.

## Memory
- **Total:** 28 GiB

## Network
- **NIC:** Realtek RTL8111/8168/8211/8411 Gigabit (onboard, `r8169`)

## Input (PTT candidate)
- **Keyboard:** Logitech Wireless ERGO K860

## Toolchain (host)
- **Compiler:** gcc 13.3.0 (alt 12 available)
- **Python:** 3.12 (system)

## GhostScribe host config

Values to set in the `server.env` for **this** host:

| Variable | Value | Why |
| --- | --- | --- |
| `GHOSTSCRIBE_COMPUTE_TYPE` | `float16` | `int8_float16` (the code default) triggers `CUBLAS_STATUS_NOT_SUPPORTED` on Blackwell (sm_120) with `ctranslate2` 4.7.1. Language detection is the first op to hit it; warm-up on silence masks the problem because VAD drops the audio before GEMM runs. Revisit when a newer ctranslate2 wheel ships Blackwell support for the int8/fp16 mixed kernel. |

