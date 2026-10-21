import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {ArrowRightLeft,SlidersHorizontal,AlertCircle,ExternalLink} from 'lucide-react';
import {ConnectHome} from './ConnectHome.jsx';
import {AdvancedSettings} from './AdvancedSettings.jsx';
const initial={platform:'macos',state:'starting',devices:[],selectedId:null,error:null,autoConnect:true,imageClipboard:true,fileClipboard:true,logs:[],appVersion:'0.4.0'};
export function App({api=invoke,refreshMs=600,preview=false}){
 const [view,setView]=useState(initial),[busy,setBusy]=useState(false),[error,setError]=useState(''),[settings,setSettings]=useState(false);const mounted=useRef(true);const settingsButton=useRef(null);
 useEffect(()=>{mounted.current=true;let timer;const poll=async()=>{try{const state=await api('get_connect_state');if(mounted.current)setView(state);}catch(e){if(mounted.current)setError(String(e));}finally{if(mounted.current)timer=setTimeout(poll,refreshMs);}};poll();return()=>{mounted.current=false;clearTimeout(timer);};},[api,refreshMs]);
 async function action(name,args={}){setError('');setBusy(true);try{await api(name,args);return true;}catch(e){if(mounted.current)setError(String(e));return false;}finally{if(mounted.current)setBusy(false);}}
 const closeSettings=()=>{setSettings(false);requestAnimationFrame(()=>settingsButton.current?.focus());};const message=error||view.error;
 return <><div className="app-frame" inert={settings||undefined}><header className="app-header"><div className="brand"><span className="brand-mark"><ArrowRightLeft size={21} strokeWidth={2.2}/></span><strong>CrossDesk</strong></div><button ref={settingsButton} className="icon-button" onClick={()=>setSettings(true)} aria-label="高级设置" title="高级设置"><SlidersHorizontal size={20}/></button></header>
 {preview&&<div className="preview-banner">界面预览 · 演示数据</div>}
 <main><ConnectHome view={view} busy={busy||view.busy} onSelect={id=>action('select_device',{id})} onPair={code=>action('pair_device',{code})} onConnect={()=>action('connect_device')} onDisconnect={()=>action('disconnect_device')} onRefresh={()=>action('refresh_devices')}/>
 {message&&<div className="error-banner" role="alert"><AlertCircle size={17}/><div><strong>{message}</strong>{view.platform==='macos'&&/权限|permission|Accessibility|accessibility/i.test(message)&&<button className="text-button" onClick={()=>action('open_permissions')}>打开辅助功能设置<ExternalLink size={12}/></button>}</div></div>}
 </main><footer className="app-footer"><span className="local-badge"><span/>局域网直连</span><span>Windows ↔ Mac · v{view.appVersion}</span></footer></div>{settings&&<AdvancedSettings view={{...view,error:message}} onClose={closeSettings} onAction={action}/>}</>;
}
