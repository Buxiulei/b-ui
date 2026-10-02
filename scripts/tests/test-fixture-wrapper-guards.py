#!/usr/bin/env python3
"""Exercise real delivery wrappers with controlled external Docker/Cargo results."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


DOCKER = r'''#!/usr/bin/env python3
import hashlib,json,os,pathlib,shutil,sys,tempfile
a=sys.argv[1:]; mode=os.environ['GUARD_CASE']
if a[0]=='inspect': print('none')
elif a[0]=='cp':
    src,dst=a[1:]
    if ':' in src:
        if mode=='cargo_failure_copy_failure': sys.exit(88)
        src=src.split(':',1)[1]
        shutil.copytree(src,dst,dirs_exist_ok=True)
    else: shutil.copytree(src,dst.split(':',1)[1],dirs_exist_ok=True)
elif 'mktemp' in a:
    p=tempfile.mkdtemp(prefix='bui-renderer-snapshot.',dir='/tmp')
    pathlib.Path(os.environ['GUARD_REMOTE']).write_text(p); print(p)
elif 'mkdir' in a:
    for p in a[a.index('700')+1:]: pathlib.Path(p).mkdir(mode=0o700)
elif 'rm' in a: shutil.rmtree(a[-1])
elif 'cargo' in a:
    snapshot=any('offline_renderer_snapshot' in s for s in a)
    name=('offline_renderer_snapshot::offline_renderer_snapshot' if snapshot else
          'modules::panel::stock_gate_fixture::stock_residential_gate_restore_and_payloads')
    if mode=='zero':
        print('running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1233 filtered out; finished in 0.00s')
        sys.exit(0)
    if mode.startswith('cargo_failure'): sys.exit(101)
    count=1
    if snapshot and mode not in ('absent','zero'):
        out=pathlib.Path(next(s.split('=',1)[1] for s in a if s.startswith('BUI_RENDER_OUTPUT_DIR=')))
        inp=pathlib.Path(next(s.split('=',1)[1] for s in a if s.startswith('BUI_RENDER_INPUT_DIR=')))
        payload=b'controlled artifact'; (out/'payload').mkdir(); (out/'payload/000.bin').write_bytes(payload)
        inventory=[{'id':'file:test','type':'File','payload':'payload/000.bin','size':19,'payload_sha256':hashlib.sha256(payload).hexdigest()}]
        compact=json.dumps(inventory,sort_keys=True,separators=(',',':')).encode()
        (out/'inventory.safe.json').write_text(json.dumps(inventory,sort_keys=True))
        (out/'plan.safe.json').write_text(json.dumps({'changes':0,'keys':1,'unchanged':1,'unknown_accesses':0}))
        proof={'input_sha256':hashlib.sha256((inp/'inputs.private.json').read_bytes()).hexdigest(),'inventory_sha256':hashlib.sha256(compact).hexdigest(),'raw_plan_empty':True,'metadata_plan_empty':True,'startup_noop':True,'current_managed_bytes_equal':True,'kernel_count':4,'unknown_accesses':0}
        (out/'proof.safe.json').write_text(json.dumps(proof))
        (out/'artifacts.private.txt').write_text('controlled full artifact')
        (out/'plan.private.txt').write_text('controlled complete Plan')
        if mode=='incomplete': (out/'payload/000.bin').unlink()
        if mode=='bad_proof': (out/'proof.safe.json').write_text('{}')
    marker=('offline_renderer_snapshot artifacts=1 kernels=4 raw_plan_empty=true metadata_plan_empty=true startup_noop=true unknown_accesses=0' if snapshot else
       'stock_gate_fixture version=1.14.2 sha256=1a60ac17d93042c5a12410cfe83472ddee5084131dfae7a9b9806926ffb84447 gates=5 active=2 denied=3 restart_new_requests=verified')
    print('running 1 test')
    if mode!='missing_marker': print(marker)
    if mode=='duplicate_marker': print(marker)
    print('test '+name+' ... ok')
    print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1232 filtered out; finished in 0.01s')
elif 'sh' in a: print('sing-box version 1.14.2')
else: sys.exit(90)
'''


class WrapperGuards(unittest.TestCase):
    def run_wrapper(self, snapshot, case):
        with tempfile.TemporaryDirectory(prefix="bui-wrapper-guard-") as directory:
            root = Path(directory)
            docker = root / "docker"
            docker.write_text(DOCKER)
            docker.chmod(0o700)
            source = Path(__file__).resolve().parent
            command = ["bash", str(source / ("test-renderer-snapshot.sh" if snapshot else "test-residential-stock-gates.sh"))]
            if snapshot:
                (root / "input").mkdir()
                (root / "input/inputs.private.json").write_text('{"controlled":true}')
                (root / "output").mkdir()
                if case == "stale": (root / "output/proof.safe.json").write_text('{}')
                command += [str(root / "input"), str(root / "output")]
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"], GUARD_CASE=case, GUARD_REMOTE=str(root / "remote"))
            result = subprocess.run(command, env=env, capture_output=True, timeout=15)
            if (root / "remote").exists():
                self.assertFalse(Path((root / "remote").read_text()).exists(), "wrapper must reap its exact temporary directory")
            return result.returncode

    def test_stock_execution_guard(self):
        for case in ("zero", "missing_marker", "duplicate_marker"):
            with self.subTest(case=case): self.assertNotEqual(self.run_wrapper(False, case), 0)
        self.assertEqual(self.run_wrapper(False, "valid"), 0)
        self.assertEqual(self.run_wrapper(False, "cargo_failure"), 101)

    def test_snapshot_execution_and_fresh_complete_output(self):
        for case in ("zero", "missing_marker", "duplicate_marker", "stale", "absent", "incomplete", "bad_proof"):
            with self.subTest(case=case): self.assertNotEqual(self.run_wrapper(True, case), 0)
        self.assertEqual(self.run_wrapper(True, "valid"), 0)
        self.assertEqual(self.run_wrapper(True, "cargo_failure"), 101)
        self.assertEqual(self.run_wrapper(True, "cargo_failure_copy_failure"), 101)


if __name__ == "__main__":
    unittest.main()
