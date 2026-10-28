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
