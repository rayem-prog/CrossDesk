// @vitest-environment jsdom
import React from 'react';
import {test,expect,vi,afterEach} from 'vitest';
import {render,screen,cleanup} from '@testing-library/react';
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
