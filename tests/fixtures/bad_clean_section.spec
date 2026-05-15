Name:           hello
Version:        1.0
Release:        1
Summary:        Demo of %clean section

License:        MIT
URL:            https://example.org/hello

%description
Body.

%clean
rm -rf %{buildroot}

%changelog
* Mon Jan 01 2024 me <me@example.org> - 1.0-1
- init
