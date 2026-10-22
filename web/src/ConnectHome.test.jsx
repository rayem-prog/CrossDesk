// @vitest-environment jsdom
import React from 'react';
import {test,expect,vi,afterEach} from 'vitest';
import {render,screen,cleanup,within} from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import {ConnectHome} from './ConnectHome.jsx';
afterEach(cleanup);
const base={platform:'macos',state:'discovering',devices:[{id:'a'.repeat(32),name:'我的 Windows',paired:false}],selectedId:null,pairingCode:null,secondsLeft:120,error:null,autoConnect:true};
function props(view){return {view,busy:false,onSelect:vi.fn(),onPair:vi.fn(),onConnect:vi.fn(),onDisconnect:vi.fn(),onRefresh:vi.fn()};}
test('first use offers a device rather than IP and key paths',()=>{
 render(<ConnectHome {...props(base)}/>);
 expect(screen.getByRole('button',{name:/我的 Windows/})).toBeTruthy();
 expect(screen.queryByLabelText(/IP|端口|密钥路径/)).toBeNull();
});
test('six digit pairing preserves leading zeroes and prevents malformed submissions',async()=>{
 const p=props({...base,state:'pairing',selectedId:'a'.repeat(32)});render(<ConnectHome {...p}/>);
 const button=screen.getByRole('button',{name:'配对并连接'});expect(button.disabled).toBe(true);
 await userEvent.type(screen.getByLabelText('配对码'),'001234');expect(button.disabled).toBe(false);
 await userEvent.click(button);expect(p.onPair).toHaveBeenCalledWith('001234');
});
test('connected view offers paste and disconnect without configuration',()=>{
 render(<ConnectHome {...props({...base,state:'connected',devices:[{...base.devices[0],paired:true}]})}/>);
 expect(screen.getByText('可以开始复制粘贴')).toBeTruthy();
 expect(screen.getByRole('button',{name:'断开连接'}).disabled).toBe(false);
 expect(screen.queryByLabelText('配对码')).toBeNull();
});
test('manual disconnect stays visible and pairing can be cancelled while busy',()=>{
 const r=render(<ConnectHome {...props({...base,state:'paused'})}/>);
 expect(screen.getByRole('button',{name:'连接设备'})).toBeTruthy();
 r.rerender(<ConnectHome {...props({...base,state:'pairing',selectedId:'a'.repeat(32)})} busy/>);
 expect(screen.getByRole('button',{name:'取消连接'}).disabled).toBe(false);
});
test('failed folder transfer stays connected and gives an actionable reason',()=>{
 render(<ConnectHome {...props({...base,state:'connected',transfer:{state:'failed',direction:'send',message:'文件超过 512 MiB'}})}/>);
 expect(screen.getByText('传输失败')).toBeTruthy();
 expect(screen.getByText('文件超过 512 MiB')).toBeTruthy();
 expect(screen.getByRole('button',{name:'断开连接'})).toBeTruthy();
});
test('an older discovered device asks for an upgrade before pairing',async()=>{
 const p=props({...base,devices:[{...base.devices[0],compatible:false}]});render(<ConnectHome {...p}/>);
 expect(screen.getByText(/请将两端升级/)).toBeTruthy();
 const button=screen.getByRole('button',{name:/我的 Windows/});expect(button.disabled).toBe(true);
 await userEvent.click(button);expect(p.onSelect).not.toHaveBeenCalled();
});
test.each([
 ['preparing','send','正在整理文件'],
 ['transferring','receive','正在传输'],
 ['waiting','send','等待对方发布剪贴板'],
 ['waiting','receive','等待写入本机剪贴板'],
 ['ready','receive','可以粘贴'],
 ['skipped','send','已跳过'],
])('shows %s transfer progress without replacing the connection status (%s)',(state,direction,label)=>{
 render(<ConnectHome {...props({...base,state:'connected',transfer:{id:'transfer-1',state,direction,message:null}})}/>);
 expect(within(screen.getByRole('status',{name:'文件与文件夹传输'})).getByText(label)).toBeTruthy();
 expect(screen.getByText('已连接')).toBeTruthy();
 expect(screen.getByRole('button',{name:'断开连接'})).toBeTruthy();
});
test('a skip explains why and is cleared when a later transfer starts',()=>{
 const r=render(<ConnectHome {...props({...base,state:'connected',transfer:{id:'old',state:'skipped',direction:'send',message:'不支持符号链接'}})}/>);
 expect(screen.getByText('不支持符号链接')).toBeTruthy();
 r.rerender(<ConnectHome {...props({...base,state:'connected',transfer:{id:'new',state:'preparing',direction:'send',message:null}})}/>);
 expect(screen.queryByText('不支持符号链接')).toBeNull();
});
test('an incompatible selection cannot submit a pairing code',()=>{
 render(<ConnectHome {...props({...base,state:'pairing',selectedId:base.devices[0].id,devices:[{...base.devices[0],compatible:false}]})}/>);
 expect(screen.queryByLabelText('配对码')).toBeNull();
 expect(screen.getByText(/请将两端升级/)).toBeTruthy();
});
test('a remembered incompatible device explains why reconnection is disabled',()=>{
 render(<ConnectHome {...props({...base,state:'paused',devices:[{...base.devices[0],paired:true,compatible:false}]})}/>);
 expect(screen.getByRole('button',{name:'连接设备'}).disabled).toBe(true);
 expect(screen.getByText(/请将两端升级/)).toBeTruthy();
});
test.each([
 ['waiting','send','等待对方写入文字剪贴板'],
 ['waiting','receive','等待写入本机文字剪贴板'],
 ['ready','receive','文字已同步'],
 ['skipped','receive','文字同步已跳过'],
 ['failed','send','文字同步失败'],
])('text %s %s has a text-specific accessible status',(state,direction,label)=>{
 render(<ConnectHome {...props({...base,state:'connected',transfer:{kind:'text',id:'text-copy',state,direction,message:'已有更新的本地复制内容；请在发送端重新复制'}})}/>);
 const status=screen.getByRole('status',{name:'文字同步'});
 expect(within(status).getByText(label)).toBeTruthy();
 expect(status.textContent).toContain('已有更新的本地复制内容');
 expect(screen.queryByRole('status',{name:'文件与文件夹传输'})).toBeNull();
 expect(screen.getByText('已连接')).toBeTruthy();
});
