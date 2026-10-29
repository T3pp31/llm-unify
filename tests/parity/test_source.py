"""Native vendoring must never restore the external Python application."""
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/parity'))
import source


class SourceSeparationTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        self.root_patch = patch.object(source, 'ROOT', self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        archive = self.root / 'reference.zip'
        with zipfile.ZipFile(archive, 'w') as output:
            output.writestr('upstream/LICENSE', 'license\r\n')
            output.writestr('upstream/litellm-rust/lib.rs', 'pub fn native() {}\r\n')
            output.writestr('upstream/litellm/__init__.py', 'raise RuntimeError("reference only")\n')
            output.writestr('upstream/ui/index.html', '<p>upstream</p>')
        self.config = {
            'version': '1', 'commit': 'fixed', 'archive': 'reference.zip',
            'archive_sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
            'archive_root': 'upstream/', 'destination': 'vendor/native',
            'include_directories': ['litellm-rust/'], 'include_files': ['LICENSE'],
            'excluded_directories': [], 'excluded_files': [],
            'reference': {'destination': 'target/reference',
                          'include_directories': ['litellm-rust/', 'litellm/', 'ui/'],
                          'include_files': ['LICENSE']},
        }

    def imported(self, config):
        count = source.import_source(config)
        manifest = self.root / config['destination'] / 'SOURCE.json'
        config['source_manifest_sha256'] = hashlib.sha256(manifest.read_bytes()).hexdigest()
        return count

    def test_default_import_keeps_only_native_inputs_and_exact_bytes(self):
        self.assertEqual(self.imported(self.config), 2)
        self.assertEqual(source.verify(self.config), 2)
        native = self.root / self.config['destination']
        self.assertFalse((native / 'litellm').exists())
        self.assertFalse((native / 'ui').exists())
        self.assertEqual((native / 'litellm-rust/lib.rs').read_bytes(), b'pub fn native() {}\r\n')

    def test_reference_import_is_explicit_and_outside_vendor(self):
        reference = source.reference_config(self.config)
        self.assertEqual(self.imported(reference), 4)
        self.assertEqual(source.verify(reference), 4)
        self.assertTrue((self.root / 'target/reference/litellm/__init__.py').exists())
        self.assertFalse((self.root / 'vendor').exists())
        self.assertEqual(self.config['destination'], 'vendor/native')

    def test_modified_native_file_fails_verification(self):
        self.imported(self.config)
        (self.root / 'vendor/native/litellm-rust/lib.rs').write_text('modified')
        with self.assertRaisesRegex(ValueError, 'hash mismatch'):
            source.verify(self.config)

    def test_extra_python_application_in_vendor_fails_verification(self):
        self.imported(self.config)
        unexpected = self.root / 'vendor/native/litellm/__init__.py'
        unexpected.parent.mkdir()
        unexpected.write_text('unexpected application')
        with self.assertRaisesRegex(ValueError, 'file set changed'):
            source.verify(self.config)

    def test_mismatched_archive_cannot_be_imported(self):
        self.config['archive_sha256'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'archive SHA-256 mismatch'):
            source.import_source(self.config)

    def test_source_manifest_cannot_be_modified(self):
        self.imported(self.config)
        path = self.root / 'vendor/native/SOURCE.json'
        data = json.loads(path.read_text())
        data['files'] = {}
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, 'manifest SHA-256 mismatch'):
            source.verify(self.config)

    def test_byte_edits_reapply_from_the_pinned_archive(self):
        original = b'pub fn native() {}\r\n'
        expected = b'pub fn renamed() { /* Unicode: \xe2\x98\x83 */ }\r\n'
        changes = {'litellm-rust/lib.rs': {
            'reason': 'Exercise reproducible byte edits without embedding old text.',
            'upstream_sha256': source.digest(original),
            'patched_sha256': source.digest(expected),
            'edits': [
                {'start': 7, 'end': 13, 'after': 'renamed'},
                {'start': 17, 'end': 17, 'after': ' /* Unicode: ☃ */ '},
            ],
        }}
        self.config['patches'] = 'patches.json'
        (self.root / 'patches.json').write_text(json.dumps({'files': changes}))
        self.assertEqual(self.imported(self.config), 2)
        self.assertEqual((self.root / 'vendor/native/litellm-rust/lib.rs').read_bytes(), expected)
        self.assertEqual(source.verify(self.config), 2)
        self.assertEqual(source.import_source(self.config), 2)

    def test_byte_edits_reject_invalid_ranges_and_wrong_hashes(self):
        original = b'abcdef'
        valid = {'start': 1, 'end': 3, 'after': 'X'}
        patch = {'upstream_sha256': source.digest(original),
                 'patched_sha256': source.digest(b'aXdef'), 'edits': [valid]}
        self.assertEqual(source.patched('fixture', original, {'fixture': patch}), b'aXdef')
        for edits in [
            [{'start': -1, 'end': 1, 'after': ''}],
            [{'start': 2, 'end': 1, 'after': ''}],
            [{'start': 0, 'end': 7, 'after': ''}],
            [{'start': True, 'end': 3, 'after': ''}],
            [valid, {'start': 2, 'end': 4, 'after': ''}],
            [valid, {'start': 0, 'end': 1, 'after': ''}],
        ]:
            with self.subTest(edits=edits), self.assertRaisesRegex(ValueError, 'invalid patch byte range'):
                source.patched('fixture', original, {'fixture': dict(patch, edits=edits)})
        with self.assertRaisesRegex(ValueError, 'source hash mismatch'):
            source.patched('fixture', b'changed', {'fixture': patch})
        with self.assertRaisesRegex(ValueError, 'result hash mismatch'):
            source.patched('fixture', original, {'fixture': dict(patch, patched_sha256='0' * 64)})
