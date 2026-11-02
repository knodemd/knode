#!/usr/bin/env python3
"""Run a synthetic CLI/MCP workflow in a disposable home. No model calls or personal config.
Requires a CLI containing the project instructions, import preview and excerpt features.
"""
import argparse
import json
import select
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--knode", type=Path, required=True, help="Path to the built CLI")
    args = parser.parse_args()
    binary = str(args.knode.resolve(strict=True))
    corpus = Path(__file__).resolve().parent / "corpus"
    with tempfile.TemporaryDirectory(prefix='knode-workflow-e2e-') as tmp:
     root=Path(tmp);(root/'bin').mkdir();(root/'user').mkdir()
     env={'HOME':str(root/'user'),'USER':'tester','KNODE_HOME':str(root/'home'),'CODEX_HOME':str(root/'codex'),'KNODE_KEY_STORE':'file','KNODE_EMBEDDINGS':'off','PATH':str(root/'bin')}
     def cli(*args):
      r=subprocess.run([binary,*map(str,args)],env=env,cwd=root,text=True,capture_output=True,timeout=30)
      if r.returncode:raise RuntimeError(r.stderr)
      return r.stdout
     def rpc_open():
      p=subprocess.Popen([binary,'mcp','--agent','codex','--space',str(space)],env=env,cwd=root,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
      clients.append(p)
      def request(i,method,params):
       p.stdin.write(json.dumps({'jsonrpc':'2.0','id':i,'method':method,'params':params})+'\n');p.stdin.flush()
       deadline=time.time()+15
       while time.time()<deadline:
        if select.select([p.stdout],[],[],1)[0]:
         line=p.stdout.readline()
         if not line:raise RuntimeError('MCP exited before response')
         value=json.loads(line)
         if value.get('id')==i:
          if 'error' in value:raise RuntimeError(value['error'])
          return value['result']
       raise RuntimeError('MCP timeout')
      request(1,'initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'isolated-workflow-test','version':'1'}})
      p.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');p.stdin.flush()
      def call(i,name,arguments):
       result=request(i,'tools/call',{'name':name,'arguments':arguments})
       if result.get('isError'):raise RuntimeError(result['content'])
       call.last_response_bytes=len(json.dumps(result,ensure_ascii=False).encode())
       return result['structuredContent']
      return p,call
     clients=[]
     try:
      cli('init','Workflow','--dir',root/'spaces','--default');space=root/'spaces/Workflow.knode'
      for name in ['api','site']:
       project=root/name;project.mkdir();cli('folders','add',project,'--kind','project','--capture','off')
      mappings=json.loads(cli('folders','list','--json'))
      assert len(mappings)==2
      assert all(not m['capture'] for m in mappings)
      report=json.loads(cli('import',corpus,'--preview','--json'))
      assert report['simulation_only'] and len(report['notes'])==5
      before=json.loads(cli('search','SQLite','--json'));assert not before['hits']
      cli('import',corpus,'--json')
      project=root/'api'
      cli('connect','codex','--instructions',project,'--print');assert not (project/'AGENTS.md').exists()
      cli('connect','codex','--instructions',project);assert (project/'AGENTS.md').exists()
      p,call=rpc_open()
      found=call(2,'search',{'query':'SQLite'})
      node=next(h['node'] for h in found['hits'] if h['node']['title']=='SQLite-decision')
      first=call(3,'read_excerpt',{'id':node['id'],'max_chars':80})
      assert first['partial'] and len(first['text'])==80
      second=call(4,'read_excerpt',{'id':node['id'],'start':first['next_start'],'snapshot':first['snapshot'],'max_chars':80})
      assert second['start']==80
      saved=call(5,'write_node',{'title':'demo-api — E2E checkpoint','body':'Deployment remains unverified. Next action: validate deployment. Project: [[Project-demo-api]].'})
      checkpoint=saved['node']['id']
      call(6,'link_nodes',{'from':checkpoint,'to':'Project-demo-api','kind':'part-of'})
      large=call(7,'write_node',{'title':'Transport volume fixture','body':'x'*100000})
      call(8,'read_node',{'id':large['node']['id']});full_bytes=call.last_response_bytes
      call(9,'read_excerpt',{'id':large['node']['id'],'max_chars':2000});excerpt_bytes=call.last_response_bytes
      assert excerpt_bytes < full_bytes and excerpt_bytes < 8000
      p.terminate();p.wait(timeout=5);clients.remove(p)
      p,call=rpc_open()
      note=call(2,'read_node',{'id':checkpoint})
      assert 'Deployment remains unverified' in note['node']['body']
      assert note['node']['created_by']['actor']=='codex'
      assert any(link['kind']=='part-of' for link in note['links'])
      p.terminate();p.wait(timeout=5);clients.remove(p)
      output={'result':'pass','synthetic_notes_previewed':5,'projects_registered_capture_off':2,'preview_left_destination_empty':True,'instruction_preview_did_not_write':True,'codex_config_and_instructions_installed_in_sandbox':True,'authenticated_excerpt_pagination':True,'checkpoint_retrieved_by_fresh_mcp_process':True,'typed_project_link_preserved':True,'personal_data_or_model_calls':False,'ascii_100k_fixture_transport_bytes':{'full_note':full_bytes,'excerpt_2000_chars':excerpt_bytes,'counts_include_text_and_structured_content':True,'not_provider_token_counts':True}}
      print(json.dumps(output,indent=2))
     finally:
      for p in clients:
       p.terminate();p.wait(timeout=5)
      cli('service','stop')


if __name__ == "__main__":
    main()
