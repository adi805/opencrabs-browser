# Aset yang ditanam ke dalam biner

## `DejaVuSans.ttf`

Face TrueType yang dipakai `native::teks` untuk mengukur lebar teks. Ditahan
di dalam repo supaya angka layout dan PNG sama di laptop, di runner CI, dan di
container: font sistem berbeda di tiap host, jadi mengukurnya dari sana membuat
kotak teks berubah tanpa satu baris kode berubah.

- Berkas: `DejaVuSans.ttf`, 759 720 byte, `unitsPerEm` 2048.
- Sumber: <https://dejavu-fonts.github.io/>, paket Debian `fonts-dejavu-core`.
- Lisensi: Bitstream Vera (permissive). Teks lisensinya wajib ikut serta:

```
Copyright (c) 2003 by Bitstream, Inc. All Rights Reserved.
Bitstream Vera is a trademark of Bitstream, Inc.
DejaVu changes are in public domain.

Permission is hereby granted, free of charge, to any person obtaining a copy
of the fonts accompanying this license ("Fonts") and associated
documentation files (the "Font Software"), to reproduce and distribute the
Font Software, including without limitation the rights to use, copy, merge,
publish, distribute, and/or sell copies of the Font Software, and to permit
persons to whom the Font Software is furnished to do so, subject to the
following conditions:

The above copyright and trademark notices and this permission notice shall
be included in all copies of one or more of the Font Software typefaces.

The Font Software may be modified, altered, or added to, and in particular
the designs of glyphs or characters in the Fonts may be modified and
additional glyphs or characters may be added to the Fonts, only if the fonts
are renamed to names not containing either the words "Bitstream" or the word
"Vera".

This License becomes null and void to the extent applicable to Fonts or Font
Software that has been modified and is distributed under the "Bitstream
Vera" names.

The Font Software may be sold as part of a larger software package but no
copy of one or more of the Font Software typefaces may be sold by itself.

THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF COPYRIGHT, PATENT,
TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL BITSTREAM OR THE GNOME
FOUNDATION BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, INCLUDING
ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL DAMAGES,
WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF
THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM OTHER DEALINGS IN THE
FONT SOFTWARE.

Except as contained in this notice, the names of Gnome, the Gnome
Foundation, and Bitstream Inc., shall not be used in advertising or
otherwise to promote the sale, use or other dealings in this Font Software
without prior written authorization from the Gnome Foundation or Bitstream
Inc., respectively. For further information, contact: fonts at gnome dot
org.
```

## `NotoSansDevanagari-Regular.ttf`

Face kedua di rantai `native::teks`, dipakai untuk aksara Devanagari. DejaVu
Sans tidak memetakan blok U+0900 sampai U+097F sama sekali, jadi tanpa face
ini seluruh teks Hindi jatuh ke `.notdef`.

- Berkas: `NotoSansDevanagari-Regular.ttf`, 229 336 byte, `unitsPerEm` 1000.
  `units_per_em`-nya sengaja berbeda dari DejaVu (2048) supaya skala yang
  dihitung dari face yang salah langsung terlihat sebagai teks 2x terlalu
  besar, bukan sebagai selisih kecil.
- Sumber: <https://github.com/notofonts/devanagari>, paket Debian
  `fonts-noto-core`.
- Lisensi: SIL Open Font License 1.1. Teks lisensinya ikut serta di
  `OFL-1.1.txt`, disalin apa adanya dari `fonts-noto-core` versi Debian.

Rantai face diurutkan "yang paling luas dulu": DejaVu memegang Latin, Yunani,
Kiril, Arab, dan Ibrani, jadi teks campuran tetap didominasi satu face dan
shaping-nya tidak terpecah tanpa perlu. Face baru ditambahkan ke
`TERTANAM` di `native/teks.rs` beserta lisensinya di folder ini.
