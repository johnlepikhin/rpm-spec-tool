Name:           hello
Version:        1.0
Release:        1
Summary:        Demo of legacy build-root variable

License:        MIT
URL:            https://example.org/hello

%description
Body.

%install
mkdir -p $RPM_BUILD_ROOT/usr/bin
install -m 755 hello $RPM_BUILD_ROOT/usr/bin/hello

%changelog
* Mon Jan 01 2024 me <me@example.org> - 1.0-1
- init
