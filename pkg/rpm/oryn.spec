# Fedora / RHEL-family spec for oryn. Builds from the GitHub release with cargo.
#   rpmbuild -ba oryn.spec        (after putting the source tarball in SOURCES/)
#   or: spectool -g -R oryn.spec && rpmbuild -ba oryn.spec
# The Cargo release profile already strips the binary, so there is no
# debuginfo to split off.
%global debug_package %{nil}
# Docs and licenses in /usr/share/{doc,licenses}/oryn, as on Fedora.
%global _docdir_fmt %{name}

Name:           oryn
Version:        0.1.9
Release:        1%{?dist}
Summary:        Terminal hardware monitor with every temperature sensor

# oryn is MIT; the rest are the Rust crates compiled into the binary
# (see LICENSES-dependencies.txt).
License:        MIT AND ISC AND Zlib AND Unicode-3.0 AND (MIT OR Apache-2.0) AND (Unlicense OR MIT) AND (Apache-2.0 OR BSL-1.0) AND (Apache-2.0 WITH LLVM-exception OR MIT OR Apache-2.0)
URL:            https://github.com/rusty725/oryn
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64
BuildRequires:  cargo >= 1.85
BuildRequires:  rust >= 1.85
BuildRequires:  gcc

Recommends:     smartmontools
Recommends:     hwdata

%description
Oryn shows every temperature sensor in the machine in one table: CPU cores,
GPU edge, hotspot and individual VRAM chips, each RAM stick, NVMe, SATA and
USB drives, chipset and network cards, each named after its hardware and with
min/max, trend and a history graph.

Around it sit btop-style panels for CPU (with live clock speed and per-core
temperatures), GPU (NVIDIA and AMD), memory, disks, network, processes (with
search, terminate, kill and nice) and system specs. 100 color schemes,
compact mode and mouse support.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --locked

%check
cargo test --release --locked

%install
install -Dm0755 target/release/oryn %{buildroot}%{_bindir}/oryn
install -Dm0644 doc/oryn.1 %{buildroot}%{_mandir}/man1/oryn.1

%files
%license LICENSE THIRD_PARTY_NOTICES.md LICENSES-dependencies.txt
%doc README.md
%{_bindir}/oryn
%{_mandir}/man1/oryn.1*

%changelog
* Sun Oct 18 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.9-1
- Specs panel: RAM speed and timings from the sticks' SPD (JEDEC, EXPO,
  XMP 3.0) and the BIOS.
- Specs panel: monitors with model, resolution, current and maximum refresh
  rate and size.

* Sun Oct 18 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.7-1
- Short windows: panels that don't fit scroll with PgUp/PgDn or the wheel
  instead of being hidden; the bottom bar lists what's above and below.

* Sat Oct 17 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.6-1
- Panel menu: clearer key hints (Ctrl+Shift+↑/↓ to move a panel).
- Tests no longer overwrite the saved settings.

* Sat Oct 17 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.5-1
- Specs: RAM part numbers for soldered memory, from SMBIOS.
- Sensors: Wi-Fi cards named from their thermal zone's driver.

* Sat Oct 17 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.4-1
- Specs: full SATA firmware revision.
- Sensors: Wi-Fi cards named from pci.ids.
- Header: version after the clock.

* Sat Oct 17 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.3-1
- Battery badge in the header on laptops, tablets and handhelds.
- Integrated GPUs get their own iGPU badge in the header.
- Specs: network adapter model and driver, GPU VBIOS and disk firmware versions.

* Sat Oct 17 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.2-1
- Integrated Intel GPUs show the shared chip temperature, labelled (iGPU).
- GPU clock reads "idle" instead of 0 MHz while the GPU sleeps.

* Sat Oct 17 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.1-1
- Intel GPUs (xe and i915) in the GPU panel.
- Terminal theme: dim text readable on Solarized and similar palettes.

* Fri Oct 16 2026 rusty725 <rusty725@users.noreply.github.com> - 0.1.0-1
- First release.
