// Loaded only by the explicit Vite development preview URL, never by release apps.
export function createPreviewApi(scene){
 let state={platform:scene==='windows'?'windows':'macos',state:scene==='connected'?'connected':scene==='windows'?'hosting':scene==='pair'?'pairing':'discovering',devices:[{id:'a'.repeat(32),name:'书房的 Windows',paired:scene==='connected'}],selectedId:scene==='pair'?'a'.repeat(32):null,pairingCode:scene==='windows'?'486291':null,secondsLeft:106,autoConnect:true,imageClipboard:true,fileClipboard:true,error:null,busy:false,logs:[],appVersion:'0.5.0'};
 return async(name,args={})=>{if(name==='select_device')state={...state,state:'pairing',selectedId:args.id};if(name==='pair_device'||name==='connect_device')state={...state,state:'connected',devices:state.devices.map(d=>({...d,paired:true}))};if(name==='disconnect_device')state={...state,state:'paused'};if(name==='set_options')state={...state,...args.options};return {...state};};
}
