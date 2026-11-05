"""Run with .usd-runtime/bin/python -m unittest discover -s tests -p usd_import_test.py."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from pxr import Usd, UsdGeom, Gf, UsdUtils
spec=importlib.util.spec_from_file_location('usd_adapter',Path(__file__).parents[1]/'src-tauri/usd/import_usd.py')
adapter=importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)

class ImportTests(unittest.TestCase):
    def test_usdc_reference_units_axis_visibility_and_usdz(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            asset=Usd.Stage.CreateNew(str(root/'asset.usdc'))
            mesh=UsdGeom.Mesh.Define(asset,'/Shape')
            mesh.CreatePointsAttr([(0,0,0),(100,0,0),(0,0,100)])
            mesh.CreateFaceVertexCountsAttr([3]);mesh.CreateFaceVertexIndicesAttr([0,1,2]);mesh.CreateSubdivisionSchemeAttr('none')
            asset.SetDefaultPrim(mesh.GetPrim());asset.GetRootLayer().Save()
            stage=Usd.Stage.CreateNew(str(root/'scene.usda'))
            UsdGeom.SetStageUpAxis(stage,'Z');UsdGeom.SetStageMetersPerUnit(stage,0.01)
            parent=UsdGeom.Xform.Define(stage,'/Root');parent.AddTranslateOp().Set(Gf.Vec3d(0,200,0))
            ref=stage.DefinePrim('/Root/Visible');ref.GetReferences().AddReference('asset.usdc')
            hidden=stage.DefinePrim('/Root/Hidden');hidden.GetReferences().AddReference('asset.usdc');UsdGeom.Imageable(hidden).CreateVisibilityAttr('invisible')
            instance=stage.DefinePrim('/Instance');instance.GetReferences().AddReference('scene.usda','/Root');instance.SetInstanceable(True)
            stage.GetRootLayer().Save()
            # Instance proxy traversal resolves the composed child mesh, including parent transforms.
            result=adapter.convert(root/'scene.usda',{'origin':'test','revision':'','license':'test'})
            self.assertEqual(len(result['meshes']),2)
            self.assertEqual(result['meshes'][0]['points'],[[0.0,0.0,-2.0],[1.0,0.0,-2.0],[0.0,1.0,-2.0]])
            UsdUtils.CreateNewUsdzPackage(str(root/'scene.usda'),str(root/'scene.usdz'))
            packed=adapter.convert(root/'scene.usdz',result['source'])
            self.assertEqual(result['meshes'],packed['meshes'])

    def test_rejects_traversal_paths(self):
        for path in ['../a','/a','x/../../a','a\\b','C:/a','a//b','a/./b']:
            with self.subTest(path=path):
                with self.assertRaises(ValueError):adapter.relative_path(path)

    def test_missing_reference_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)/'broken.usda'
            root.write_text('#usda 1.0\ndef Xform "Root" (prepend references = @missing.usdc@) {}')
            with self.assertRaises(ValueError):adapter.convert(root,{'origin':'test','revision':'','license':'test'})

class ProgressTests(unittest.TestCase):
    def test_download_chunks_report_bytes_and_enforce_limit(self):
        import io
        from unittest.mock import patch
        payload = b'x' * 150000
        class Response(io.BytesIO):
            headers = {'Content-Length': str(len(payload))}
        events = []
        with patch.object(adapter.urllib.request, 'urlopen', return_value=Response(payload)), patch.object(adapter, 'progress', side_effect=lambda *a, **k: events.append(a)):
            self.assertEqual(adapter.read_url('https://example.invalid/test', len(payload)), payload)
        self.assertTrue(any(event[1] == 65536 for event in events))
        self.assertEqual(events[-1], ('read', len(payload), len(payload)))
        with patch.object(adapter.urllib.request, 'urlopen', return_value=Response(payload)):
            with self.assertRaises(ValueError): adapter.read_url('https://example.invalid/test', 100)

    def test_status_is_small_atomic_numeric_metadata(self):
        import json
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'progress.json'
            with patch.object(adapter, '_progress_path', path):
                adapter.progress('extract', 4096, 10000, force=True)
                status = json.loads(path.read_text())
            self.assertEqual(status, {'phase':'extract', 'completed':4096, 'total':10000})
            self.assertFalse(path.with_suffix('.tmp').exists())

class PackageTests(unittest.TestCase):
    def test_manifest_pins_revision_checks_hashes_and_keeps_dependencies(self):
        from unittest.mock import patch
        import hashlib,json
        commit='a'*40
        files={'scene.usda':b'#usda 1.0\n', 'textures/albedo.bin':b'asset-bytes', '.metadata/LICENSE.txt':b'CC-BY-4.0'}
        bom=json.dumps({'items':[{'relative_path':p,'size':len(b),'hash':{'sha256':hashlib.sha256(b).hexdigest()}} for p,b in files.items()]}).encode()
        manifest=json.dumps({'format_version':'1.0','license':'CC-BY-4.0','metadata':[{'name':'com.nvidia.simready.packaging.bom.json','hash':{'sha256':hashlib.sha256(bom).hexdigest()}}]}).encode()
        root='https://huggingface.co/datasets/test/assets/resolve/'+commit+'/chair/'
        responses={root+p:b for p,b in files.items()}
        responses[root+'com.nvidia.simready.packaging.json']=manifest
        responses[root+'.metadata/com.nvidia.simready.packaging.bom.json']=bom
        responses[root+'.metadata/com.nvidia.simready.root_usds.json']=json.dumps({'entries':['scene.usda']}).encode()
        calls=[]
        def fetch(url,limit):
            calls.append(url)
            return responses[url]
        with tempfile.TemporaryDirectory() as tmp, patch.object(adapter,'json_url',return_value={'sha':commit}),patch.object(adapter,'read_url',side_effect=fetch):
            path,source=adapter.download_package('https://huggingface.co/datasets/test/assets/blob/main/chair/com.nvidia.simready.packaging.json',Path(tmp)/'package')
            self.assertEqual(path.read_bytes(),files['scene.usda'])
            self.assertEqual(source['revision'],commit)
            self.assertEqual((path.parent/'textures/albedo.bin').read_bytes(),b'asset-bytes')
            self.assertTrue(all('/resolve/'+commit+'/' in url for url in calls))
            responses[root+'scene.usda']=b'corrupted'
            with self.assertRaisesRegex(ValueError,'hash mismatch'):
                adapter.download_package('https://huggingface.co/datasets/test/assets/resolve/main/chair/com.nvidia.simready.packaging.json',Path(tmp)/'corrupt')

    def test_rejects_non_package_and_external_urls(self):
        for url in ['https://huggingface.co/spaces/nvidia/simready-central','https://evil.example/datasets/test/a/resolve/main/a/com.nvidia.simready.packaging.json','https://huggingface.co/datasets/test/a/resolve/main/../com.nvidia.simready.packaging.json']:
            with self.subTest(url=url), self.assertRaises(ValueError):adapter.download_package(url,Path('/unused'))

if __name__=='__main__':unittest.main()
