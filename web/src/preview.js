// Loaded only by the explicit Vite development preview URL, never by release apps.
export function createPreviewApi(scene){
 let state={platform:scene==='windows'?'windows':'macos',state:scene==='connected'?'connected':scene==='windows'?'hosting':scene==='pair'?'pairing':'discovering',devices:[{id:'a'.repeat(32),name:'书房的 Windows',paired:scene==='connected'}],selectedId:scene==='pair'?'a'.repeat(32):null,pairingCode:scene==='windows'?'486291':null,secondsLeft:106,autoConnect:true,textClipboard:true,textClipboardActive:null,imageClipboard:true,fileClipboard:true,error:null,busy:false,logs:[],appVersion:'0.7.1',diagnostics:{run_id:null,state:'idle',remaining_seconds:0,report:null}};
 let desktop={backgroundAvailable:false,startAtLogin:false,error:null};let startedAt=0,runId=0;
 const finish=reason=>{state={...state,diagnostics:{...state.diagnostics,state:reason,remaining_seconds:0,report:{elapsed_ms:Math.min(60000,Date.now()-startedAt),stages:[],coalesced_samples:0,max_queue_depth:0}}};};
 return async(name,args={})=>{
 if(name==='set_drop_edge')state={...state,dropEdge:args.edge};
 if(name==='get_desktop_state')return {...desktop};
 if(name==='set_start_at_login'){desktop={...desktop,startAtLogin:args.enabled};return {...desktop};}
 if(name==='start_input_diagnostics'&&state.state==='connected'){startedAt=Date.now();state={...state,diagnostics:{run_id:++runId,state:'collecting',remaining_seconds:60,report:null}};}
 if(name==='stop_input_diagnostics'&&state.diagnostics.state==='collecting')finish('stopped');
 if(state.diagnostics.state==='collecting'){const remaining=Math.max(0,60-Math.floor((Date.now()-startedAt)/1000));if(remaining===0)finish('completed');else state={...state,diagnostics:{...state.diagnostics,remaining_seconds:remaining}};}
 if(name==='export_input_diagnostics')return null;
if(name==='select_device')state={...state,state:'pairing',selectedId:args.id};if(name==='pair_device'||name==='connect_device')state={...state,state:'connected',devices:state.devices.map(d=>({...d,paired:true}))};if(name==='disconnect_device')state={...state,state:'paused'};if(name==='set_options')state={...state,...args.options};return {...state};};
}

// Explicit development-only drop surface fixtures; no filesystem or native commands.
export function createDropPreviewBoundary(scene='drop') {
 const state=scene==='saved'?'saved':scene==='receiving'?'receiving':'waiting';
 const transfer={id:'preview-drop',state,names:['旅行照片','行程.pdf','路线图.png'],totalBytes:184549376,transferredBytes:scene==='receiving'?83047219:scene==='saved'?184549376:0,message:null,savedPath:scene==='saved'?'/preview/received':null};
 let snapshot={edge:'left',peerName:'书房的 Windows',drop:{enabled:true,peerSupported:true,peerEnabled:true,channel:'ready',incoming:transfer,outgoing:scene==='edge'?{...transfer,state:'sending'}:null},error:null};
 const listeners=new Map();
 const emit=()=>listeners.get('crossdesk-drop')?.({payload:structuredClone(snapshot)});
 return {
  events:async(name,handler)=>{listeners.set(name,handler);if(name==='crossdesk-drop-expanded')queueMicrotask(()=>handler({payload:true}));return ()=>listeners.delete(name);},
  api:async(name)=>{
   if(name==='get_drop_state')return structuredClone(snapshot);
   if(name==='accept_drop'){snapshot={...snapshot,drop:{...snapshot.drop,incoming:{...transfer,state:'receiving',transferredBytes:83047219}}};emit();return true;}
   if(name==='reject_drop'||name==='cancel_drop'){snapshot={...snapshot,drop:{...snapshot.drop,incoming:{...transfer,state:name==='reject_drop'?'rejected':'cancelled'}}};emit();}
   if(name==='dismiss_drop'){snapshot={...snapshot,drop:{...snapshot.drop,incoming:null,outgoing:null}};emit();}
  }
 };
}
