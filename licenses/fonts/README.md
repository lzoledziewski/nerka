# Fonts in nerka-gui

`nerka-gui` contains egui's built-in fonts (crate `epaint_default_fonts`
0.37). Their licences, copied from that crate, ship with every release:

| Font | Licence | File |
|---|---|---|
| Ubuntu Light | Ubuntu Font Licence 1.0 | [UFL.txt](UFL.txt) |
| Noto Emoji | SIL Open Font License 1.1 | [OFL.txt](OFL.txt) |
| Hack | MIT (Hack), public domain (DejaVu changes), Bitstream Vera Licence | [Hack-Regular.txt](Hack-Regular.txt) |
| emoji-icon-font | MIT | [emoji-icon-font-mit-license.txt](emoji-icon-font-mit-license.txt) |
| egui-icons (a subset of emoji-icon-font) | MIT | [egui-icons.txt](egui-icons.txt) |

Update these files when egui is updated (`cargo deny check` lists the
crate's licences).
