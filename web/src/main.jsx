import React from 'react';
import {createRoot} from 'react-dom/client';
import {App} from './App.jsx';
import './style.css';
const root=createRoot(document.getElementById('root'));
if(import.meta.env.DEV&&new URLSearchParams(location.search).has('preview')){
 import('./preview.js').then(({createPreviewApi})=>root.render(<App preview api={createPreviewApi(new URLSearchParams(location.search).get('preview'))}/>));
}else root.render(<App/>);
