Name:           hello
Version:        1.0
Release:        1
Summary:        Demo of shellcheck findings
License:        MIT
URL:            https://example.org/hello

%description
Body.

%install
FOO=$1
echo $FOO
rm -rf %{buildroot}/usr/bin/$missing_quote

%changelog
* Mon Jan 01 2024 me <me@example.org> - 1.0-1
- init
