import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {listen} from '@tauri-apps/api/event';
import {ArrowRightLeft,SlidersHorizontal,AlertCircle,ExternalLink} from 'lucide-react';
import {ConnectHome} from './ConnectHome.jsx';
import {AdvancedSettings} from './AdvancedSettings.jsx';
const initial={platform:'macos',state:'starting',devices:[],selectedId:null,error:null,autoConnect:true,textClipboard:true,textClipboardActive:null,imageClipboard:true,fileClipboard:true,logs:[],appVersion:'0.7.1'};
export function App({api=invoke,refreshMs=600,preview=false}){
 const [view,setView]=useState(initial),[busy,setBusy]=useState(false),[error,setError]=useState(''),[settings,setSettings]=useState(false);const mounted=useRef(true);const settingsButton=useRef(null);const refresh=useRef(()=>{});const [desktop,setDesktop]=useState({backgroundAvailable:false,startAtLogin:false});const [desktopBusy,setDesktopBusy]=useState(false);const desktopGeneration=useRef(0);
 useEffect(()=>{
  mounted.current=true;let disposed=false,timer,inflight=false,pending=false,nativeVisible=true,visible=!document.hidden,generation=0,nativeEvents=0,unlisten;
  const active=()=>!disposed&&visible;
  const poll=async()=>{if(!active())return;if(inflight){pending=true;return;}clearTimeout(timer);inflight=true;const requestGeneration=generation;
   try{const state=await api('get_connect_state');if(active()&&generation===requestGeneration)setView(state);}
   catch(e){if(active()&&generation===requestGeneration)setError(String(e));}
   finally{inflight=false;if(active()){if(pending){pending=false;poll();}else timer=setTimeout(poll,refreshMs);}}
  };
  const visibility=()=>{const next=nativeVisible&&!document.hidden;if(next===visible)return;visible=next;generation++;clearTimeout(timer);pending=false;if(visible)poll();};
  const refreshNow=()=>{if(active())poll();};refresh.current=refreshNow;
  document.addEventListener('visibilitychange',visibility);
  if(!preview)Promise.resolve().then(()=>listen('crossdesk-visibility',event=>{nativeEvents++;nativeVisible=event.payload!==false;visibility();})).then(async stop=>{
   if(disposed){stop();return;}unlisten=stop;const queryVersion=nativeEvents;
   try{const current=await api('get_window_visibility');if(!disposed&&queryVersion===nativeEvents&&typeof current==='boolean'){nativeVisible=current;visibility();}}catch{}
  }).catch(()=>{});
  poll();return()=>{disposed=true;mounted.current=false;generation++;clearTimeout(timer);document.removeEventListener('visibilitychange',visibility);unlisten?.();if(refresh.current===refreshNow)refresh.current=()=>{};};
 },[api,refreshMs,preview]);
 useEffect(()=>{let active=true;const generation=++desktopGeneration.current;api('get_desktop_state').then(state=>{if(active&&generation===desktopGeneration.current&&typeof state?.startAtLogin==='boolean')setDesktop(state);}).catch(error=>{if(active&&generation===desktopGeneration.current&&!/unknown command|not found|not implemented|__TAURI|undefined/i.test(String(error)))setDesktop(current=>({...current,error:String(error)}));});return()=>{active=false;};},[api,settings]);
 async function setLogin(enabled){desktopGeneration.current++;setDesktopBusy(true);setError('');try{const state=await api('set_start_at_login',{enabled});if(mounted.current){if(typeof state?.startAtLogin!=='boolean')throw new Error('无法确认登录启动设置');setDesktop(state);if(state.error)setError(state.error);}}catch(e){if(mounted.current)setError(String(e));}finally{if(mounted.current)setDesktopBusy(false);}}

 async function action(name,args={}){setError('');setBusy(true);try{const result=await api(name,args);refresh.current();return name==='export_input_diagnostics'?result:true;}catch(e){if(mounted.current)setError(String(e));return false;}finally{if(mounted.current)setBusy(false);}}
 const closeSettings=()=>{setSettings(false);requestAnimationFrame(()=>settingsButton.current?.focus());};const message=error||view.error||view.dropError||desktop.error;
 return <><div className="app-frame" inert={settings||undefined}><header className="app-header"><div className="brand"><span className="brand-mark"><ArrowRightLeft size={21} strokeWidth={2.2}/></span><strong>CrossDesk</strong></div><button ref={settingsButton} className="icon-button" onClick={()=>setSettings(true)} aria-label="高级设置" title="高级设置"><SlidersHorizontal size={20}/></button></header>
 {preview&&<div className="preview-banner">界面预览 · 演示数据</div>}
 <main><div className="background-actions"><button className="text-button" disabled={!desktop.backgroundAvailable||busy} onClick={()=>action('hide_to_background')}>后台运行</button><small>关闭窗口将退出应用</small></div><ConnectHome view={view} busy={busy||view.busy} onSelect={id=>action('select_device',{id})} onPair={code=>action('pair_device',{code})} onConnect={()=>action('connect_device')} onDisconnect={()=>action('disconnect_device')} onRefresh={()=>action('refresh_devices')}/>
 {message&&<div className="error-banner" role="alert"><AlertCircle size={17}/><div><strong>{message}</strong>{view.platform==='macos'&&/权限|permission|Accessibility|accessibility/i.test(message)&&<button className="text-button" onClick={()=>action('open_permissions')}>打开辅助功能设置<ExternalLink size={12}/></button>}</div></div>}
 </main><footer className="app-footer"><span className="local-badge"><span/>局域网直连</span><span>Windows ↔ Mac · v{view.appVersion}</span></footer></div>{settings&&<AdvancedSettings view={{...view,error:message}} desktop={desktop} desktopBusy={desktopBusy} onSetLogin={setLogin} onClose={closeSettings} onAction={action}/>}</>;
}
