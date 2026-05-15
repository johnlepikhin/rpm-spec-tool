Name:           macro-fold-demo
Version:        1.0
Release:        1
License:        MIT
Summary:        Demonstrates RPM117 (Phase 8c macro-defined-makes-if-trivial)
URL:            https://example.invalid/

%global with_python 1

%if %{with_python}
BuildRequires:  python3
%endif

%description
Test fixture for RPM117. `with_python` is defined as `1` above, so the
`%if %{with_python}` test always succeeds — the wrapper is redundant.

%prep

%build

%install

%files

%changelog
* Mon Jan 01 2024 Tester <tester@example.invalid> - 1.0-1
- Initial release.
