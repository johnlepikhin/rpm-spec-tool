Name:           hello
Version:        1.0
Release:        1
Summary:        Spec without %prep

License:        MIT
URL:            https://example.org/hello

%build
make

%install
make install DESTDIR=%{buildroot}

%description
Body.

%changelog
* Mon Jan 01 2024 me <me@example.org> - 1.0-1
- init
