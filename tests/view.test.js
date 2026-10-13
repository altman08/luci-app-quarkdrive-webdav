'use strict';

const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const test = require('node:test');

const source = readFileSync(resolve(__dirname,
	'../luci-app-quarkdrive-webdav/htdocs/luci-static/resources/view/quarkdrive-webdav.js'), 'utf8');
String.prototype.format = function(...args) {
	return this.replace(/%s/g, () => String(args.shift()));
};

function deferred() {
	let resolve, reject;
	const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
	return { promise, resolve, reject };
}

async function setup({ enabled = '0', running = false, readonly = false } = {}) {
	const env = {
		enabled, running, readonly, calls: [], notices: [], buttons: {}, events: {}, options: {}, tabs: [],
		formValues: { enabled, host: '0.0.0.0', port: '8080', quark_cookie: 'test-cookie' },
		cached: { enabled, host: '0.0.0.0', port: '8080' }, saves: 0, applies: [], resets: 0, renders: 0
	};
	const E = (tag, attrs = {}, children) => {
		const node = { tag, attrs, children, style: {}, value: '', scrollTop: 0, clientHeight: 100, scrollHeight: 100 };
		if (tag === 'button') env.buttons[children] = node;
		return node;
	};
	class Map {
		constructor() { this.readonly = readonly; }
		section() {
			return {
				tab: name => env.tabs.push(name),
				taboption: (tab, type, key) => (env.options[key] = {
					map: this, tab, option: key, disabled: '0', formvalue: () => env.formValues[key]
				})
			};
		}
		lookupOption(key) { return [env.options[key]]; }
		render() { env.renders++; return Promise.resolve({}); }
		save() {
			env.saves++;
			return Promise.resolve(env.saveReply).then(() => {
				env.enabled = env.formValues.enabled;
				Object.assign(env.cached, env.formValues);
			});
		}
		reset() { env.resets++; return Promise.resolve(); }
	}
	const rpc = { declare: ({ object, method }) => (...args) => {
		env.calls.push({ object, method, args });
		return Promise.resolve().then(() => {
			if (object === 'uci') {
				assert.deepEqual(args, ['quarkdrive-webdav', 'config', 'enabled']);
				if (env.enabledError) throw new Error('UCI offline');
				return env.enabled;
			}
			if (env.statusReply) return env.statusReply();
			return { 'quarkdrive-webdav': { instances: {
				main: { running: env.running, pid: env.running ? 123 : undefined },
				old: { running: false, pid: 456 }
			} } };
		});
	} };
	const fs = { exec: (path, args) => {
		env.calls.push({ object: 'file', method: 'exec', args });
		assert.equal(path, '/etc/init.d/quarkdrive-webdav');
		if (args[0] === 'log') return Promise.resolve({ code: 0, stdout: 'log entry' });
		return Promise.resolve(env.actionReply || { code: 0 }).then(reply => {
			if (reply.code === 0) env.running = args[0] !== 'stop_now';
			return reply;
		});
	} };
	const ui = {
		addNotification: (title, node, level) => env.notices.push({ node, level }),
		changes: { apply: checked => {
			env.applies.push(checked);
			env.running = env.enabled === '1';
			return Promise.resolve();
		} }
	};
	const document = { getElementById: () => true, addEventListener: (name, handler) => { env.events[name] = handler; } };
	const window = { location: { hostname: 'router.lan' }, open: (...args) => { env.opened = args; } };
	const uci = { load: () => Promise.resolve(), get: (config, section, key) => env.cached[key] };
	env.view = new Function('view', 'form', 'uci', 'rpc', 'poll', 'fs', 'ui', 'dom', 'E', '_', 'document', 'window', source)(
		{ extend: value => value }, { Map }, uci, rpc, { add: (callback, interval) => {
			assert.equal(interval, 5);
			env.poll = callback;
		} }, fs, ui, { content: (node, children) => { node.children = children; } }, E, value => value, document, window);
	await env.view.load();
	await env.view.render();
	env.status = () => env.options._status.renderWidget().children;
	env.click = name => env.buttons[name].attrs.click({ currentTarget: env.buttons[name] });
	env.execs = () => env.calls.filter(call => call.object === 'file' && call.args[0] !== 'log').map(call => call.args[0]);
	return env;
}

for (const [enabled, running, label, style] of [
	['0', false, 'Not enabled', 'label warning'],
	['1', false, 'Stopped', 'label'],
	['1', true, 'Running', 'label success'],
	['0', true, 'Running (not enabled)', 'label warning']
]) {
	test(`state enabled=${enabled}, running=${running} has the correct label and controls`, async () => {
		const env = await setup({ enabled, running });
		assert.deepEqual(env.tabs, ['status', 'basic', 'advanced', 'log']);
		assert.equal(env.options.enabled.tab, 'status');
		assert.equal(env.status()[0].children, label);
		assert.equal(env.status()[0].attrs.class, style);
		assert.equal(env.buttons.Start.disabled, running);
		assert.equal(env.buttons.Stop.disabled, !running);
		assert.equal(env.buttons.Restart.disabled, !running);
		assert.equal(env.buttons.Open.disabled, !running);
		assert.equal(env.buttons.Start.className, 'cbi-button cbi-button-apply');
		assert.equal(env.buttons.Stop.className, 'cbi-button cbi-button-reset');
		assert.equal(env.buttons.Restart.className, 'cbi-button cbi-button-reload');
		if (running) assert.equal(env.status()[1].children, 'PID: 123');
	});
}

test('manual start, restart and stop use temporary commands and preserve unsaved form values', async () => {
	const env = await setup();
	env.formValues.quark_cookie = 'unsaved edits';
	await env.click('Start');
	assert.equal(env.status()[0].children, 'Running (not enabled)');
	await env.click('Restart');
	await env.click('Stop');
	assert.deepEqual(env.execs(), ['start_now', 'restart_now', 'stop_now']);
	assert.equal(env.enabled, '0');
	assert.equal(env.formValues.quark_cookie, 'unsaved edits');
	assert.equal(env.renders, 1);
	assert.equal(env.saves, 0);
	assert.equal(env.status()[0].children, 'Not enabled');
	assert.equal(env.notices.length, 3);
});

test('unsaved checkbox changes do not affect the status, while applied settings are refreshed', async () => {
	const env = await setup({ running: true });
	env.formValues.enabled = '1';
	await env.poll();
	assert.equal(env.status()[0].children, 'Running (not enabled)');
	env.enabled = '1';
	await env.events['uci-applied']();
	assert.equal(env.status()[0].children, 'Running');
	assert.equal(env.cached.enabled, '0');
	assert.equal(env.renders, 1);
});

test('save leaves runtime alone and apply follows enabled in both directions', async () => {
	const env = await setup();
	env.formValues.enabled = '1';
	await env.view.handleSave();
	assert.equal(env.running, false);
	assert.equal(env.status()[0].children, 'Stopped');
	assert.deepEqual(env.applies, []);
	await env.view.handleSaveApply(null, '0');
	assert.equal(env.status()[0].children, 'Running');
	env.formValues.enabled = '0';
	await env.view.handleSaveApply(null, '1');
	assert.equal(env.status()[0].children, 'Not enabled');
	assert.deepEqual(env.applies, [true, false]);
	assert.deepEqual(env.execs(), []);
});

test('pending service actions exclude polling, repeated actions, saves, applies and reset', async () => {
	const env = await setup();
	const pending = deferred();
	env.actionReply = pending.promise;
	const operation = env.click('Start');
	const count = env.calls.length;
	await Promise.all([env.click('Start'), env.poll(), env.view.handleSave(), env.view.handleSaveApply(null, '0'), env.view.handleReset()]);
	assert.equal(env.calls.length, count);
	assert.equal(env.saves, 0);
	assert.equal(env.resets, 0);
	assert.deepEqual(env.applies, []);
	assert.ok(Object.values(env.buttons).every(button => button.disabled));
	pending.resolve({ code: 0 });
	await operation;
	assert.equal(env.buttons.Stop.disabled, false);
});

test('an old poll cannot overwrite the status after a service action', async () => {
	const env = await setup();
	const pending = deferred();
	env.statusReply = () => pending.promise;
	const oldPoll = env.poll();
	await Promise.resolve();
	env.statusReply = null;
	await env.click('Start');
	pending.resolve({});
	await oldPoll;
	assert.equal(env.status()[0].children, 'Running (not enabled)');
	assert.equal(env.buttons.Stop.disabled, false);
});

test('failed status or enabled queries disable control without claiming a stopped service', async () => {
	const env = await setup({ enabled: '1', running: true });
	for (const failure of ['service', 'uci']) {
		env.statusReply = failure === 'service' ? () => { throw new Error('offline'); } : null;
		env.enabledError = failure === 'uci';
		await env.poll();
		assert.equal(env.status().children, 'Unable to read service status.');
		assert.ok(Object.values(env.buttons).every(button => button.disabled));
	}
});

test('a failed action reports its error and allows retry without changing saved settings', async () => {
	const env = await setup();
	env.actionReply = { code: 1, stderr: 'QuarkDrive cookie is required' };
	await env.click('Start');
	assert.equal(env.notices[0].node.children, 'QuarkDrive cookie is required');
	assert.notEqual(env.notices[0].level, 'info');
	assert.equal(env.buttons.Start.disabled, false);
	assert.equal(env.enabled, '0');
});

test('read-only accounts can view status and open WebDAV but cannot mutate settings or service', async () => {
	const env = await setup({ enabled: '1', running: true, readonly: true });
	await Promise.all([env.click('Start'), env.click('Stop'), env.click('Restart'), env.view.handleSave(), env.view.handleSaveApply(null, '0')]);
	assert.deepEqual(env.execs(), []);
	assert.equal(env.saves, 0);
	assert.equal(env.buttons.Open.disabled, false);
	env.click('Open');
	assert.equal(env.opened[0], 'http://router.lan:8080/');
});

test('failed saves do not apply settings and release the operation guard', async () => {
	const env = await setup();
	const pending = deferred();
	env.saveReply = pending.promise;
	const save = env.view.handleSaveApply(null, '0');
	await env.click('Start');
	assert.deepEqual(env.execs(), []);
	pending.reject(new Error('invalid settings'));
	await assert.rejects(save, /invalid settings/);
	assert.deepEqual(env.applies, []);
	assert.equal(env.buttons.Start.disabled, false);
});
