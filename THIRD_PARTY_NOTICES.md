# Third-party notices

Oryn includes code adapted from the projects below. Their copyright notices
and licenses are reproduced here, as those licenses require.

## LACT

`src/nvapi.rs`: the NvAPI call IDs, structure layouts, register offsets
and decoding for NVIDIA hotspot, memory and per-chip VRAM temperatures are
ported from LACT (lact-daemon/src/server/gpu_controller/nvidia/nvapi.rs).

<https://github.com/ilya-zlobintsev/LACT>

```text
MIT License

Copyright (c) 2023 Ilya Zlobintsev

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## nvidia-gpu-sensors

`src/nvapi.rs`: the RTX 50 (Blackwell) hotspot scan, i.e. the NV_THERM
register window bounds, the temperature-word tag check and the chip-ID check, is
ported from nvidia-gpu-sensors.

<https://github.com/philipl/nvidia-gpu-sensors>

```text
MIT License

Copyright (c) 2026 Philip Langdale

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## btop

No btop code is included. Oryn's panel design is modelled on btop: the CPU,
memory, disks, network and process panels, superscript toggle keys in panel
titles, and the process list's action bar. btop is
Copyright 2021 Aristocratos, licensed under the Apache License 2.0.

<https://github.com/aristocratos/btop>
