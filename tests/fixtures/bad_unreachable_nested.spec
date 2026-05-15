Name:           dead-branch-demo
Version:        1.0
Release:        1
License:        MIT
Summary:        Demonstrates a dead nested %if branch
URL:            https://example.invalid/

%if !X
%if X
BuildArch:      noarch
%endif
%endif

%description
Test fixture for RPM113 (Phase 8b unreachable-branch-under-parent).

%prep

%build

%install

%files

%changelog
* Mon Jan 01 2024 Tester <tester@example.invalid> - 1.0-1
- Initial release.
