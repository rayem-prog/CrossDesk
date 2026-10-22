import React,{useEffect,useState} from 'react';
import {Monitor,Laptop,ArrowRight,Check,Wifi,RefreshCw,LoaderCircle,Link2,Image,File,Type,ShieldCheck} from 'lucide-react';
const labels={starting:'正在准备',discovering:'寻找设备',pairing:'首次配对',hosting:'等待配对',searching:'寻找已配对设备',connecting:'正在连接',listening:'等待 Mac 连接',retrying:'正在重新连接',connected:'已连接',paused:'已断开',error:'需要处理'};
const transferLabels={preparing:'正在整理文件',transferring:'正在传输',ready:'可以粘贴',skipped:'已跳过',failed:'传输失败'};
export function ConnectHome({view,busy=false,onSelect,onPair,onConnect,onDisconnect,onRefresh}){
 const [code,setCode]=useState('');useEffect(()=>setCode(''),[view.selectedId]);
 const windows=view.platform==='windows';const connected=view.state==='connected';const paired=view.devices?.find(d=>d.paired);const selected=view.devices?.find(d=>d.id===view.selectedId);const pairing=!connected&&!windows&&!paired&&Boolean(view.selectedId)&&selected?.compatible!==false;
 const transfer=view.transfer;const textTransfer=transfer?.kind==='text';const textLabels={preparing:'正在准备文字',transferring:'正在同步文字',waiting:transfer?.direction==='send'?'等待对方写入文字剪贴板':'等待写入本机文字剪贴板',ready:'文字已同步',skipped:'文字同步已跳过',failed:'文字同步失败'};const transferLabel=textTransfer?textLabels[transfer.state]:transfer?.state==='waiting'?(transfer.direction==='send'?'等待对方发布剪贴板':'等待写入本机剪贴板'):transferLabels[transfer?.state];
 const pending=busy||['starting','connecting','retrying'].includes(view.state);
 let title=connected?'可以开始复制粘贴':windows?(paired?'等待你的 Mac':'让 Mac 找到这台电脑'):paired?'连接已记住的电脑':pairing?'输入 Windows 上的配对码':'找到你的 Windows';
 return <section className="connect-home">
  <div className={`status-tag ${connected?'online':''}`} role="status"><span className="status-dot"/>{labels[view.state]||'正在准备'}</div>
  <h1>{title}</h1><p className="intro">{connected?'文字、截图、图片、文件和文件夹，复制后就能在另一端粘贴。':paired?'已经记住这台设备，之后打开 CrossDesk 就能连接。':'两端保持在同一局域网。配对一次，以后打开就连。'}</p>
  <div className={`computer-link ${connected?'linked':''}`} aria-hidden="true">
   <div className="computer"><div className="computer-icon windows"><Monitor size={43} strokeWidth={1.45}/><span className="mini-os">▦</span></div><span>{windows?'这台 Windows':(paired?.name||selected?.name||'Windows')}</span></div>
   <div className="link-wire"><span/>{connected?<Check size={20}/>:pending?<LoaderCircle className="spin" size={20}/>:<Link2 size={20}/>}<span/></div>
   <div className="computer"><div className="computer-icon mac"><Laptop size={47} strokeWidth={1.45}/></div><span>{windows?'你的 Mac':'这台 Mac'}</span></div>
  </div>
  {!connected&&windows&&!paired&&<div className="pair-card">
   <div className="card-caption"><ShieldCheck size={16}/>只需首次配对</div>
   {view.pairingCode?<><p>在 Mac 上选择这台电脑，输入</p><output className="pair-code" aria-label="此电脑的配对码">{view.pairingCode.slice(0,3)}<span> </span>{view.pairingCode.slice(3)}</output><small>配对码剩余 {view.secondsLeft} 秒有效</small></>:<><p>配对码已失效</p><button className="button primary" onClick={onRefresh}>重新生成配对码</button></>}
  </div>}
  {!connected&&!windows&&!paired&&!pairing&&<div className="device-section"><div className="section-label"><span>附近的 Windows</span><button className="text-button" onClick={onRefresh} disabled={busy}><RefreshCw size={13}/>刷新</button></div>
   {view.devices?.length?view.devices.map(device=><button className="device-row" key={device.id} onClick={()=>onSelect(device.id)} disabled={busy||device.compatible===false} aria-label={`连接到 ${device.name}`}><span className="device-thumb"><Monitor size={22}/></span><span className="device-details"><strong>{device.name}</strong><small>{device.compatible===false?'版本不兼容，请将两端升级后再连接':'局域网设备 · 点击配对'}</small></span><ArrowRight size={18}/></button>):<div className="discovery-empty"><LoaderCircle size={20} className="spin"/><div><strong>正在寻找 Windows…</strong><p>请在另一台电脑上打开 CrossDesk</p></div></div>}
  </div>}
  {pairing&&<form className="pair-card input-card" onSubmit={e=>{e.preventDefault();if(/^\d{6}$/.test(code)&&!busy)onPair(code);}}><label htmlFor="pair-code">配对码</label><input autoFocus id="pair-code" type="text" inputMode="numeric" autoComplete="one-time-code" maxLength={11} placeholder="000 000" value={code} disabled={busy} onChange={e=>setCode(e.target.value.replace(/[ -]/g,''))}/><button type="submit" className="button primary full" disabled={busy||!/^\d{6}$/.test(code)}>{busy?<LoaderCircle size={17} className="spin"/>:<Link2 size={17}/>}配对并连接</button><small>只输入 Windows 当前显示的六位数字</small></form>}
  {(connected||paired||['paused','error'].includes(view.state))&&<div className="connection-actions">{connected?<button className="button secondary" onClick={onDisconnect}>断开连接</button>:['paused','error'].includes(view.state)?<button className="button primary" onClick={onConnect} disabled={busy||paired?.compatible===false}><Link2 size={17}/>连接设备</button>:<button className="button secondary" onClick={onDisconnect}>取消连接</button>}</div>}
  {pairing&&<button className="text-button cancel" onClick={onDisconnect}>取消连接</button>}
  {!connected&&paired?.compatible===false&&<p className="compatibility-note">版本不兼容，请将两端升级后再连接。</p>}
  {transferLabel&&<div className={`transfer-status ${transfer.state}`} role="status" aria-label={textTransfer?"文字同步":"文件与文件夹传输"}><strong>{transferLabel}</strong>{transfer.message&&<p>{transfer.message}</p>}</div>}
  {connected&&<div className="sync-note"><span><Type size={15}/>{view.textClipboard===false?'文字同步已关闭':'文字同步开启'}</span><i/><span><Image size={15}/>{view.imageClipboard===false?'图片同步已关闭':'图片同步开启'}</span><i/><span><File size={15}/>{view.fileClipboard===false?'文件与文件夹同步已关闭':'文件与文件夹同步开启'}</span></div>}
  {connected&&view.textClipboard!==false&&view.textClipboardActive===false&&<p className="compatibility-note">对端未开启或不支持文字同步</p>}
  {!connected&&<div className="permission-note"><Wifi size={15}/><span>{windows?'首次出现防火墙提示时，允许此应用访问专用网络。':'首次使用时，请允许辅助功能与本地网络访问。'}</span></div>}
 </section>;
}
