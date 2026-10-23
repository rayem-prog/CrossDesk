import React,{useState} from 'react';
const stages=[['send_queue','发送排队'],['send_write','网络写入'],['receive_queue','接收排队'],['inject','系统注入'],['pacing_late','发送节奏迟到']];
export function InputDiagnostics({view,onAction}){
 const diagnostics=view.diagnostics||{state:'idle',remaining_seconds:0,report:null};
 const [path,setPath]=useState('');
 const collecting=diagnostics.state==='collecting',report=diagnostics.report;
 const duration=value=>Number.isFinite(value)?`${(value/1000).toFixed(3)} ms`:'暂无数据';
 return <section className="input-diagnostics" aria-labelledby="diagnostics-title"><h3 id="diagnostics-title">输入诊断</h3><p>采集本机输入处理各阶段耗时，不代表端到端延迟。采样持续 60 秒，无需重新连接。</p>
 <div className="inline-actions"><button className="button secondary" disabled={view.state!=='connected'||collecting} onClick={()=>{setPath('');onAction('start_input_diagnostics');}}>开始 60 秒采样</button><button className="button secondary" disabled={!collecting} onClick={()=>onAction('stop_input_diagnostics')}>停止采样</button></div>
 <p role="status">{collecting?`采样中 · 剩余 ${diagnostics.remaining_seconds} 秒`:diagnostics.state==='completed'?'采样完成':diagnostics.state==='stopped'?'采样已停止':'尚未采样'}</p>
 {report&&<><p>采样时长 {(report.elapsed_ms/1000).toFixed(1)} 秒 · 合并样本 {report.coalesced_samples} · 队列峰值 {report.max_queue_depth}</p><div className="diagnostics-table-wrap"><table><thead><tr><th>阶段</th><th>样本数</th><th>P50</th><th>P95</th><th>P99</th><th>最大值</th></tr></thead><tbody>{stages.map(([name,label])=>{const stage=report.stages?.find(item=>item.name===name);return <tr key={name}><th scope="row">{label}</th>{stage?.count>0?<><td>{stage.count}</td><td>{duration(stage.p50_us)}</td><td>{duration(stage.p95_us)}</td><td>{duration(stage.p99_us)}</td><td>{duration(stage.max_us)}</td></>:<td colSpan={5}>暂无数据</td>}</tr>;})}</tbody></table></div></>}
 <button className="text-button" disabled={!report||collecting} onClick={async()=>{const saved=await onAction('export_input_diagnostics');if(typeof saved==='string')setPath(saved);}}>导出 JSON</button>{path&&<p className="export-path" role="status">{path}</p>}</section>;
}
