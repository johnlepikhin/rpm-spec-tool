Name:           hello
Version:        1.0
Release:        1
Summary:        Spec with two %build sections

License:        MIT
URL:            https://example.org/hello

%prep
%setup -q

%build
make

%install
make install DESTDIR=%{buildroot}

%build
make extra

%description
Body.

%changelog
* Mon Jan 01 2024 me <me@example.org> - 1.0-1
- init
