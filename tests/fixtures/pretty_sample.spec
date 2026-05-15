Name:           sample
Version:        1.0
Release:        1
Summary:        Pretty fixture exercising major token kinds
License:        MIT
URL:            https://example.org/sample

%if 0%{?fedora}
BuildRequires:  gcc
%endif

%description
Sample spec for the `pretty` subcommand CLI tests.

%files
/usr/bin/sample

%changelog
* Mon Jan 01 2024 Maintainer <m@example.org> - 1.0-1
- Initial packaging.
