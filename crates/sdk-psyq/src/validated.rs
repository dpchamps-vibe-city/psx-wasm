//! The SDK objects the adapters were written against and tested on, as
//! (member, identity key): `psxw-inputs::identity` keys, the sha256 of the
//! member's placed text with relocated bytes masked (M8). The build binds
//! an adapter only to a function of one of these objects; any other object
//! is translated (or the build fails, if the function itself touches
//! hardware). Add an object here once its adapters pass conformance (or
//! the oracles) against it.
//!
//! PsyQ 4.7 (btn's and bof4's links; signature database `470`). Validated
//! by the adapters' unit tests against the 4.7 objects and psyq-decomp's
//! sources, `psxw conform` (scenarios, and the chip cases in both games),
//! and the whole-game oracles.

pub const VALIDATED: &[(&str, &str)] = &[
    ("libapi/c112", "6d011673eb04bf3a24a7326b3e0c93884881d829922f15cd3c1829b2c7dcbd4f"),
    ("libapi/counter", "d336fc039bbcdedbcc156aa2f3ebdb2e6c1a67b68441f518221b39f8ef47f398"),
    ("libapi/first", "5a9847288fd7f6e40001cc523db9cb8a8ac4b81b273d4f2e930ece31f539cbac"),
    ("libapi/pad", "ac0ecfda8db404c439851e8b7220e92c5db0f7db5e7968d68661768e1c1b5529"),
    ("libapi/patch", "331463f1e79c46d81eef446547cb5b3045c3acbd526f1ad3a132e6aa1113747a"),
    ("libcard/a74", "7cfd68eefaba92fdc3389626a5aa4ef88237b50ad00d42c9a2ae8f28d70f1333"),
    ("libcard/a75", "3201dbd265d1f569cf4baf14ae09238125f26cc61e03ebfe31e51f5faf83600d"),
    ("libcard/a76", "dc68b38cc61ea46be7fd2afa6debf2528cfbc6fd5b9ec83e656fe9279c58a244"),
    ("libcard/init", "faeec493734cf307852f58fd8f8bd00fc02705d4ad36037985866770dd821dc6"),
    ("libcd/cdr_1", "804326710f09169461dc40687088a55ba76c0edbe0fc3af1ed41606a36bd9057"),
    ("libcd/event", "06bd6e87bc79b295e0989b21d2d9bc98faf092a8ea6a76d6bf7a25886881e9f1"),
    ("libcd/iso9660", "81115b193d15274c432059634ccb97f73b11326970b25d9b720f2416d93c766d"),
    ("libcd/s_003", "b0e38e7630ef5cae3f21348c977fa6390de99a81c9eb8f6a4ae706c99b69eb4a"),
    ("libcd/s_004", "b0e38e7630ef5cae3f21348c977fa6390de99a81c9eb8f6a4ae706c99b69eb4a"),
    ("libcd/s_005", "b0e38e7630ef5cae3f21348c977fa6390de99a81c9eb8f6a4ae706c99b69eb4a"),
    ("libcd/s_008", "20c3a8daad2ca1ef691428fa27cad0dd0e0113eeae8224c3066e369cf4b7a5f1"),
    ("libcd/s_012", "20c3a8daad2ca1ef691428fa27cad0dd0e0113eeae8224c3066e369cf4b7a5f1"),
    ("libcd/s_013", "20c3a8daad2ca1ef691428fa27cad0dd0e0113eeae8224c3066e369cf4b7a5f1"),
    ("libcd/s_014", "100629d4c181bd660bac7db13ece1132c4c83240a601a5cb6810cd1311173f0d"),
    ("libcd/s_015", "100629d4c181bd660bac7db13ece1132c4c83240a601a5cb6810cd1311173f0d"),
    ("libcd/s_016", "c8ffe171bb7dd796ed6aa5a789a0074384655ca3c3401a37d7e7755bbc887ec4"),
    ("libcd/s_021", "d765636c71f72f252321924479ff32eed4b8fdb1a9b842d109995c6ec86bbbb6"),
    ("libcd/s_022", "d765636c71f72f252321924479ff32eed4b8fdb1a9b842d109995c6ec86bbbb6"),
    ("libcd/s_023", "b551454f7b9bbe671b4e68d1d7f67f69159c213379a4620c901f0428df98e823"),
    ("libcd/s_024", "20c3a8daad2ca1ef691428fa27cad0dd0e0113eeae8224c3066e369cf4b7a5f1"),
    ("libetc/intr", "09b0feea2c632554bb610534332bc10cf1708c337d9cfd65247ac9400a35799e"),
    ("libetc/vsync", "5ced3e577ea12a54481e823fd26a28b831c29d427c4728d4c35233f3c46095a8"),
    ("libgpu/sys", "02b22171147b3ca173f009f2cd944c513074ba6db33d956ed051c67cdba18e04"),
    ("libmcx/mcxcmd2", "58e691a13329379c176821716b5baf24e9e9c75b1429e4970cd620e00efc6d5e"),
    ("libmcx/mcxcmd23", "92804c396fb540fcda4c0e0081118fd7ebd1ca6bc816cefd95be8b1ac8a9a7bc"),
    ("libmcx/mcxcmd24", "9926ec52d6649a8cebb51eaca0bad13eb020056a15b5bdf8b8e5b989a2c16f02"),
    ("libmcx/mcxinit", "207348a4d4a238743aeaa735bfdac5f1d7899a7c87d4c6039159e40685919d65"),
    ("libmcx/mcxmain", "0613d35f2626d3c3fd435be6119f3483c4cf1aa6035b98b1c130317047025d55"),
];
