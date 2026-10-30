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
    ("libcd/bios_1", "cb9a61afdd902c961cb4e96191dbff413192dcac5db58b3c5f0514022ac37996"),
    ("libcd/bios_2", "65b6bae4a35efaa57b4fc13c05e36241da2836035e86e944c035bd49d7fc34db"),
    ("libcd/bios_3", "c84be05de2f6bd0ffb2a54a0c4114ad8ff22920c2ce8d1460396b1a06a2314c7"),
    ("libcd/c_003", "dd7223578f0a19736d3f37d70d070e8834816e8679219a6e655be23b358e35cf"),
    ("libcd/c_011", "b32c02a2f0d941f45e346ad1873bf59a88dd4f53d2bbdcc66fabef75de0862d8"),
    ("libetc/intr", "09b0feea2c632554bb610534332bc10cf1708c337d9cfd65247ac9400a35799e"),
    ("libetc/vsync", "5ced3e577ea12a54481e823fd26a28b831c29d427c4728d4c35233f3c46095a8"),
    ("libgpu/sys", "02b22171147b3ca173f009f2cd944c513074ba6db33d956ed051c67cdba18e04"),
    // libmcx's adapters (McxStartCom, _mcxDsrEntry), and the objects whose
    // code and data they read (the elements, the vblank handler,
    // _mcxStepSio, the bulk transfer's words).
    ("libmcx/mcxdsr", "a566997a1397665ea2126a77ab548e3ee5cf68e2aa32f22a764e5fb96621e877"),
    ("libmcx/mcxinit", "207348a4d4a238743aeaa735bfdac5f1d7899a7c87d4c6039159e40685919d65"),
    ("libmcx/mcxmain", "0613d35f2626d3c3fd435be6119f3483c4cf1aa6035b98b1c130317047025d55"),
    ("libmcx/mcxresi", "287468efa03dffc7e83b6e96967958b4ad9ca31cca3019bd631dbbf74d4a8737"),
    // libpad's calls, and the vblank engine behind them (pddirres,
    // pdresres), which the adapters reproduce.
    ("libpad/pdcmd1", "65c474183d32625a87f9200f812955e6e5a2a9fcf54a333891e7bcf07cfa3232"),
    ("libpad/pdcmd2", "ec5a41120721b65782d3d4141e202e6a017c21a1fd1a238b4bcdc23c7fb423a9"),
    ("libpad/pddirini", "02dbfaddb39ba68986c0288ad97accced6cc0abb01bcd3e0f716afe3ef781b37"),
    ("libpad/pddirres", "94965b212e2647077d77c7b94572f65be8ac996e72d5e91a3c25b286293cdd1b"),
    ("libpad/pdent2", "657636a8ac63dd77546bcc1a6a091be2ae72e720306aeacbf42fdc7988f35e0a"),
    ("libpad/pdent3", "7d2897d55e85c6baa151a0c941b652c08c249d884715441d3e6a6de54d9c1c25"),
    ("libpad/pdmaiini", "e18ba4711badfc36e67e6915d33234c40e111f65113f360848256adeb65bf86d"),
    ("libpad/pdresres", "f513971cf3615a3314cdba2bd6b63e7640e1bbe0c5316786d64174ba65c1da29"),
    // libpress's MDEC functions (bof4 places it in an overlay).
    ("libpress/libpress", "c750d3ede6ee193e0d881ed3ded57994c4f32625514c0cb9f53f447d62d6e879"),
];
