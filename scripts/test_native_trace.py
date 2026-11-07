"""python3 -m unittest discover -s scripts -p 'test_*.py'"""
import json
import tempfile
import unittest
from pathlib import Path

from native_trace import Tracer, errors, item_ranges, masked

PRISTINE = """struct MeshTables { base: u8, layers: u8 }
pub fn resolve(x: u8) -> u8 {
    x
}
"""
# One mod's helper inside a target (formatted with a trailing comma, an array type's `;` in its signature), another's
# support module; braces in strings, chars and comments must not end them early.
STAGED = """struct MeshTables { base: u8, layers: u8 }
pub fn resolve(x: u8) -> u8 {
    if let Some(v) = {
        fn fast_mesh_resolve(x: u8, _at: [f64; 3],) -> Option<u8> {
            let _ = "}"; let _ = '}'; // }
            let _ = r#"}"#;
            Some(MeshTables { base: x }.base)
        }
        fast_mesh_resolve(x, [0.0; 3])
    } {
        return v;
    }
    x
}
mod carry_tables {
    pub fn lift<'a>(t: &'a super::MeshTables) -> u8 { t.missing }
}
fn untouched() -> u8 { MeshTables { base: 1 }.base }
"""


def span(text, needle, file="src/main.rs"):
    start = STAGED.encode().index(needle.encode())
    return {"file_name": file, "byte_start": start, "byte_end": start + len(needle), "is_primary": True}


class Trace(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        (self.dir / "stage/src").mkdir(parents=True)
        (self.dir / "pristine/src").mkdir(parents=True)
        (self.dir / "stage/src/main.rs").write_text(STAGED)
        (self.dir / "pristine/src/main.rs").write_text(PRISTINE)
        self.plan([
            {"mod_id": "fast_mesh", "mod_version": "0.1.0", "file": "src/main.rs",
             "helper_source": "fn fast_mesh_resolve(x: u8) -> Option<u8> { Some(MeshTables { base: x }.base) }"},
        ], [{"mod_id": "carryblock", "mod_version": "0.1.2", "file": "src/main.rs", "name": "carry_tables"}])
        self.mods = {("fast_mesh", "0.1.0"), ("carryblock", "0.1.2")}

    def plan(self, injections, modules):
        (self.dir / "stage/.horizon-mixins-stage.json").write_text(
            json.dumps({"schema": 6, "injections": injections, "modules": modules}))

    def tracer(self, mods=None):
        return Tracer(self.dir / "stage", self.dir / "pristine", mods or self.mods)

    def test_an_error_in_a_helper_names_its_mod_though_formatting_changed_it(self):
        self.assertEqual(self.tracer().owner(span(STAGED, "MeshTables { base: x }")), "fast_mesh")

    def test_an_error_in_a_support_module_names_its_mod(self):
        self.assertEqual(self.tracer().owner(span(STAGED, "t.missing")), "carryblock")

    def test_an_error_in_the_generated_call_to_a_helper_names_its_mod(self):
        self.assertEqual(self.tracer().owner(span(STAGED, "[0.0; 3]")), "fast_mesh")

    def test_an_error_in_horizons_own_code_names_no_mod(self):
        self.assertIsNone(self.tracer().owner(span(STAGED, "MeshTables { base: 1 }")))
        self.assertIsNone(self.tracer().owner(span(STAGED, "return v")))

    def test_a_path_through_another_crate_folder_resolves(self):
        (self.dir / "stage/lib").mkdir()
        s = span(STAGED, "t.missing", file="lib/../src/main.rs")
        self.assertEqual(self.tracer().owner(s), "carryblock")

    def test_a_function_the_pristine_source_has_is_not_a_mods(self):
        (self.dir / "pristine/src/main.rs").write_text(PRISTINE + "fn fast_mesh_resolve() {}\n")
        self.assertIsNone(self.tracer().owner(span(STAGED, "MeshTables { base: x }")))

    def test_a_mod_not_built_or_two_owners_name_no_mod(self):
        self.assertIsNone(self.tracer({("carryblock", "0.1.2")}).owner(span(STAGED, "MeshTables { base: x }")))
        self.plan([{"mod_id": m, "mod_version": "1", "file": "src/main.rs",
                    "helper_source": "fn fast_mesh_resolve() {}"} for m in ("a", "b")], [])
        self.assertIsNone(self.tracer({("a", "1"), ("b", "1")}).owner(span(STAGED, "MeshTables { base: x }")))

    def test_braces_in_literals_and_comments_are_not_code(self):
        code = masked(STAGED.encode())
        (start, end), = item_ranges(code, "fn", "fast_mesh_resolve")
        self.assertTrue(STAGED.encode()[start:end].endswith(b"}.base)\n        }\n        fast_mesh_resolve(x, [0.0; 3])\n    }"))
        self.assertEqual(len(code), len(STAGED.encode()))

    def test_cargo_json_errors(self):
        out = "\n".join([
            "not json",
            json.dumps({"reason": "compiler-artifact"}),
            json.dumps({"reason": "compiler-message", "message": {"level": "warning", "message": "w", "spans": []}}),
            json.dumps({"reason": "compiler-message", "message": {
                "level": "error", "message": "missing fields", "code": {"code": "E0063"}, "rendered": "error[E0063]",
                "spans": [{"file_name": "a.rs", "is_primary": True}, {"file_name": "b.rs", "is_primary": False}]}}),
        ])
        (only,) = errors(out)
        self.assertEqual((only["code"], [s["file_name"] for s in only["spans"]]), ("E0063", ["a.rs"]))


if __name__ == "__main__":
    unittest.main()
