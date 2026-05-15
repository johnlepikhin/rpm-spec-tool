Name:           main
Version:        1.0
Release:        1
Summary:        Subpackage self-obsoletion demo

License:        MIT
URL:            https://example.org/main

%description
Body.

%package -n foo
Summary:        Subpackage standalone
Obsoletes:      foo

%description -n foo
Subpackage body.

%changelog
* Mon Jan 01 2024 me <me@example.org> - 1.0-1
- init
