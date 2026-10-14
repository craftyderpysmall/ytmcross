# Built by .copr/Makefile, which fills in Version and Release.
Name:           ytmcross
Version:        0.1.0
Release:        1%{?dist}
Summary:        A minimal, fast, native YouTube Music client
License:        MIT
URL:            https://github.com/craftyderpysmall/ytmcross
Source0:        %{name}-%{version}.tar.gz
# Every crate, vendored, with the .cargo/config.toml that uses them.
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo >= 1.88
BuildRequires:  gcc
BuildRequires:  desktop-file-utils
BuildRequires:  pkgconfig(alsa)
BuildRequires:  pkgconfig(gtk+-3.0)
BuildRequires:  pkgconfig(webkit2gtk-4.1)

# The release profile strips the binaries.
%global debug_package %{nil}

%description
A YouTube Music client written in Rust: native UI drawn on the CPU, playback
in-process, no Electron and no bundled browser. ytmcross-login shows the
one-time Google sign-in window.

%prep
%autosetup -a 1

%build
cargo build --release --locked --offline

%install
install -Dm755 target/release/ytmcross %{buildroot}%{_bindir}/ytmcross
install -Dm755 target/release/ytmcross-login %{buildroot}%{_bindir}/ytmcross-login
install -Dm644 assets/ytmcross.desktop %{buildroot}%{_datadir}/applications/ytmcross.desktop
install -Dm644 assets/ytmcross.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/ytmcross.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/ytmcross.desktop

%files
%doc README.md
%{_bindir}/ytmcross
%{_bindir}/ytmcross-login
%{_datadir}/applications/ytmcross.desktop
%{_datadir}/icons/hicolor/scalable/apps/ytmcross.svg

%changelog
* Wed Oct 14 2026 craftyderpysmall <craftyderpysmall@gmail.com> - 0.1.1-1
- App icon

* Wed Oct 14 2026 craftyderpysmall <craftyderpysmall@gmail.com> - 0.1.0-1
- First package
