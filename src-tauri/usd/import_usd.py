"""Official OpenUSD adapter. Executed by Rust, never by the WebView."""
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import sys
import time
import urllib.parse
import urllib.request

MAX_PACKAGE = 512 * 1024 * 1024
MAX_FILES = 2048
MAX_POINTS = 1500000


# Small atomic status file. Rust attaches its operation ID; no asset names enter IPC.
_progress_path = None
_progress_time = 0.0
_progress_phase = None

def progress(phase, completed=0, total=0, force=False):
    global _progress_time, _progress_phase
    if _progress_path is None:
        return
    now = time.monotonic()
    if not force and phase == _progress_phase and now - _progress_time < 0.1:
        return
    temporary = _progress_path.with_suffix('.tmp')
    temporary.write_text(json.dumps({'phase': phase, 'completed': completed, 'total': total}))
    os.replace(temporary, _progress_path)
    _progress_time, _progress_phase = now, phase


def relative_path(value):
    if not isinstance(value, str) or not value or '\\' in value or ':' in value:
        raise ValueError('Invalid package path')
    p = PurePosixPath(value)
    if p.is_absolute() or any(x in ('', '.', '..') for x in value.split('/')):
        raise ValueError('Unsafe package path')
    return p


def read_url(url, limit):
    # Only constructed Hugging Face endpoints are requested. CDN redirects serve LFS.
    with urllib.request.urlopen(url, timeout=30) as response:
        total = int(response.headers.get('Content-Length', '0'))
        progress('read', 0, total, force=True)
        data = bytearray()
        while len(data) <= limit:
            chunk = response.read(min(65536, limit + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
            progress('read', len(data), max(total, len(data)))
        progress('read', len(data), len(data), force=True)
    if len(data) > limit:
        raise ValueError('Download exceeds limit: ' + urllib.parse.urlsplit(url).path)
    return bytes(data)


def json_url(url):
    return json.loads(read_url(url, 4 * 1024 * 1024))


def download_package(url, destination):
    parsed = urllib.parse.urlsplit(url)
    parts = urllib.parse.unquote(parsed.path).strip('/').split('/')
    if (parsed.scheme != 'https' or parsed.netloc != 'huggingface.co' or parsed.query
            or parsed.fragment or len(parts) < 7 or parts[0] != 'datasets'
            or parts[3] not in ('resolve', 'blob')
            or parts[-1] != 'com.nvidia.simready.packaging.json'):
        raise ValueError('Use a Hugging Face dataset URL ending in com.nvidia.simready.packaging.json')
    for part in parts:
        relative_path(part)
    repo = '/'.join(parts[1:3])
    revision = parts[4]
    folder = '/'.join(parts[5:-1])
    quote = lambda p: urllib.parse.quote(p, safe='/')
    info = json_url('https://huggingface.co/api/datasets/' + quote(repo) + '/revision/' + quote(revision) + '?expand[]=sha')
    commit = info['sha']
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('Invalid dataset revision')
    base = 'https://huggingface.co/datasets/' + quote(repo) + '/resolve/' + commit + '/' + quote(folder) + '/'
    manifest_data = read_url(base + parts[-1], 4 * 1024 * 1024)
    manifest = json.loads(manifest_data)
    if manifest.get('format_version') != '1.0':
        raise ValueError('Unsupported SimReady package version')
    destination.mkdir(parents=True)
    (destination / parts[-1]).write_bytes(manifest_data)
    bom_name = 'com.nvidia.simready.packaging.bom.json'
    descriptor = next((x for x in manifest.get('metadata', []) if x.get('name') == bom_name), None)
    if not descriptor:
        raise ValueError('Package has no bill of materials')
    bom_bytes = read_url(base + '.metadata/' + bom_name, 4 * 1024 * 1024)
    if hashlib.sha256(bom_bytes).hexdigest() != descriptor['hash']['sha256']:
        raise ValueError('Bill of materials hash mismatch')
    (destination / '.metadata').mkdir()
    (destination / '.metadata' / bom_name).write_bytes(bom_bytes)
    items = json.loads(bom_bytes)['items']
    if len(items) > MAX_FILES or sum(x['size'] for x in items) > MAX_PACKAGE:
        raise ValueError('Package exceeds 512 MiB / 2048 files')
    seen = set()
    for item in items:
        rel = relative_path(item['relative_path'])
        if str(rel) in seen or not isinstance(item['size'], int) or not 0 <= item['size'] <= MAX_PACKAGE:
            raise ValueError('Invalid or duplicate package entry')
        seen.add(str(rel))
        digest = item.get('hash', {}).get('sha256', '')
        if not re.fullmatch(r'[0-9a-f]{64}', digest):
            raise ValueError('Package entry requires SHA-256')
        data = read_url(base + quote(str(rel)), item['size'])
        if len(data) != item['size'] or hashlib.sha256(data).hexdigest() != digest:
            raise ValueError('Package file hash mismatch: ' + str(rel))
        target = destination / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    roots_path = destination / '.metadata/com.nvidia.simready.root_usds.json'
    if not roots_path.exists():
        # Some packages store this discovery metadata outside the BOM.
        roots_path.write_bytes(read_url(base + '.metadata/com.nvidia.simready.root_usds.json', 65536))
    roots = json.loads(roots_path.read_text())['entries']
    if len(roots) != 1:
        raise ValueError('Package has multiple entry stages; download it and import a local USD file')
    root = destination / relative_path(roots[0])
    if not root.is_file() or root.suffix.lower() not in ('.usd', '.usda', '.usdc', '.usdz'):
        raise ValueError('Invalid package entry stage')
    return root, {'origin': base + parts[-1], 'revision': commit, 'license': str(manifest.get('license', 'Unspecified'))}


def convert(path, source):
    from pxr import Usd, UsdGeom, UsdShade, Gf
    progress("parse", force=True)
    stage = Usd.Stage.Open(str(path), load=Usd.Stage.LoadAll)
    if not stage:
        raise ValueError('Unable to open USD stage')
    errors = stage.GetCompositionErrors()
    if errors:
        raise ValueError('Unresolved USD composition: ' + str(errors[0]))
    time = Usd.TimeCode(stage.GetStartTimeCode()) if stage.HasAuthoredTimeCodeRange() else Usd.TimeCode.Default()
    scale = UsdGeom.GetStageMetersPerUnit(stage)
    up = UsdGeom.GetStageUpAxis(stage)
    if not math.isfinite(scale) or scale <= 0 or up not in ('Y', 'Z'):
        raise ValueError('Invalid stage units or up axis')
    cache = UsdGeom.XformCache(time)
    meshes = []
    warnings = {'static_snapshot', 'flat_shading'}
    total_points = total_indices = 0
    progress("convert", force=True)
    for index, prim in enumerate(Usd.PrimRange.Stage(stage, Usd.TraverseInstanceProxies())):
        if index % 128 == 0:
            progress("convert", index, 0)
        if not prim.IsA(UsdGeom.Mesh):
            if prim.IsA(UsdGeom.PointInstancer) or prim.IsA(UsdGeom.Curves) or prim.IsA(UsdGeom.Gprim):
                warnings.add('unsupported_geometry')
            continue
        mesh = UsdGeom.Mesh(prim)
        if mesh.ComputeVisibility(time) == UsdGeom.Tokens.invisible or mesh.ComputePurpose() in ('guide', 'proxy'):
            continue
        points = mesh.GetPointsAttr().Get(time)
        counts = mesh.GetFaceVertexCountsAttr().Get(time)
        indices = mesh.GetFaceVertexIndicesAttr().Get(time)
        if not points or not counts or not indices:
            continue
        total_points += len(points)
        total_indices += len(indices)
        if total_points > MAX_POINTS or total_indices > MAX_POINTS * 3 or len(meshes) >= 1000:
            raise ValueError('USD geometry exceeds import budget')
        matrix = cache.GetLocalToWorldTransform(prim)
        transformed = []
        progress("extract", 0, len(points), force=True)
        for index, p in enumerate(points):
            if index % 4096 == 0:
                progress("extract", index, len(points))
            v = matrix.Transform(Gf.Vec3d(p)) * scale
            transformed.append([v[0], v[2], -v[1]] if up == 'Z' else list(v))
        progress("extract", len(points), len(points), force=True)
        color = [0.58, 0.62, 0.67]
        display = mesh.GetDisplayColorPrimvar().ComputeFlattened(time)
        if display:
            color = list(display[0])
            if len(display) > 1:
                warnings.add('varying_color')
        material, _ = UsdShade.MaterialBindingAPI(prim).ComputeBoundMaterial()
        if material:
            warnings.add('materials_approximate')
            shader, _, _ = material.ComputeSurfaceSource()
            if shader and shader.GetIdAttr().Get() == 'UsdPreviewSurface':
                inp = shader.GetInput('diffuseColor')
                if inp and not inp.HasConnectedSource() and inp.Get(time) is not None:
                    color = list(inp.Get(time))
        if mesh.GetSubdivisionSchemeAttr().Get() != 'none':
            warnings.add('subdivision_cage')
        meshes.append({'name':str(prim.GetName()),'prim_path':str(prim.GetPath()),'points':transformed,
                       'counts':list(counts),'indices':list(indices),'holes':list(mesh.GetHoleIndicesAttr().Get(time) or []),
                       'left_handed': mesh.GetOrientationAttr().Get() == 'leftHanded', 'color':color})
    if not meshes:
        raise ValueError('USD contains no supported visible polygon meshes')
    warnings.add('simulation_metadata_only')
    return {'meshes':meshes,'source':source,'warnings':sorted(warnings),'meters_per_unit':scale,'up_axis':str(up)}


def main():
    global _progress_path
    request = json.load(sys.stdin)
    if request.get("progress"):
        _progress_path = Path(request["progress"])
    source = {'origin': request['source'], 'revision': '', 'license': 'See source asset license'}
    path = Path(request['source'])
    if request['source'].startswith('https://'):
        path, source = download_package(request['source'], Path(request['package_dir']))
    elif path.suffix.lower() not in ('.usd','.usda','.usdc','.usdz') or not path.is_file():
        raise ValueError('Select a .usd, .usda, .usdc or .usdz file')
    result = convert(path, source)
    progress('convert', force=True)
    with open(request['output'], 'w') as f:
        json.dump(result, f, allow_nan=False, separators=(',', ':'))


if __name__ == '__main__':
    try:
        main()
    except Exception as e:
        print(str(e), file=sys.stderr)
        sys.exit(1)
