'use strict';
'require view';
'require form';
'require uci';
'require rpc';
'require poll';
'require fs';
'require ui';

const serviceName = 'quarkdrive-webdav';
const initPath = '/etc/init.d/' + serviceName;
const callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: ['name'],
	expect: { '': {} }
});

function absolutePath(sectionId, value) {
	return !value || value.charAt(0) === '/' || _('Use an absolute path beginning with /.');
}

return view.extend({
	load() {
		return uci.load(serviceName);
	},

	render() {
		let m, s, o;
		const status = E('strong', {}, _('Collecting data…'));
		const logs = E('textarea', {
			'class': 'cbi-input-textarea',
			'readonly': '',
			'rows': 18,
			'style': 'width:100%;font-family:monospace'
		});
		const controls = [];
		const serviceButtons = {};
		let running = null;
		let busy = false;
		let statusRequest = 0;
		const setButtonState = (button, disabled, style) => {
			button.disabled = !!disabled;
			button.className = 'btn cbi-button' + (disabled ? '' : ' ' + style);
		};
		const updateButtons = () => {
			Object.keys(serviceButtons).forEach(action => {
				const item = serviceButtons[action];
				const disabled = busy || m.readonly || running === null ||
					(action === 'start' && running) || (action === 'stop' && !running);
				setButtonState(item.button, disabled, item.style);
			});
			setButtonState(openButton, busy || running !== true, 'cbi-button-action');
		};
		const refreshStatus = () => {
			const request = ++statusRequest;
			return callServiceList(serviceName).then(result => {
				if (request !== statusRequest)
					return;
				const instances = (result[serviceName] || {}).instances || {};
				running = Object.keys(instances).some(key => instances[key].running);
				status.textContent = running ? _('RUNNING') : _('NOT RUNNING');
				status.style.color = running ? 'green' : 'red';
			}).catch(() => {
				if (request !== statusRequest)
					return;
				running = null;
				status.textContent = _('Unable to read service status.');
				status.style.color = '';
			}).finally(updateButtons);
		};
		const runAction = (action, button) => {
			if (button.disabled || busy || m.readonly)
				return Promise.resolve();
			busy = true;
			++statusRequest;
			updateButtons();
			return fs.exec(initPath, [action]).then(result => {
				if (result.code !== 0)
					throw new Error(result.stderr || _('Service operation failed.'));
			}).catch(error => {
				ui.addNotification(null, E('p', {}, error.message));
			}).finally(() => refreshStatus().finally(() => {
				busy = false;
				updateButtons();
			}));
		};
		[
			['start', _('Start'), 'cbi-button-positive'],
			['stop', _('Stop'), 'cbi-button-negative'],
			['restart', _('Restart'), 'cbi-button-reload']
		].forEach(item => {
			const button = E('button', {
				'class': 'btn cbi-button ' + item[2],
				'type': 'button',
				'click': ev => runAction(item[0], ev.currentTarget)
			}, item[1]);
			serviceButtons[item[0]] = { button: button, style: item[2] };
			controls.push(button, ' ');
		});
		const openButton = E('button', {
			'class': 'btn cbi-button cbi-button-action',
			'type': 'button',
			'click': () => {
				if (openButton.disabled)
					return;
				let host = uci.get(serviceName, 'config', 'host') || '0.0.0.0';
				if (host === '0.0.0.0' || host === '::' || host === '[::]')
					host = window.location.hostname;
				if (host.indexOf(':') !== -1 && host.charAt(0) !== '[')
					host = '[' + host + ']';

				const url = new URL('http://' + host);
				url.port = uci.get(serviceName, 'config', 'port') || '8080';
				const prefix = uci.get(serviceName, 'config', 'strip_prefix') || '';
				url.pathname = prefix.split('/').map(encodeURIComponent).join('/').replace(/\/+$/, '') + '/';
				window.open(url.href, '_blank', 'noopener,noreferrer');
			}
		}, _('Open'));
		controls.push(openButton);

		m = new form.Map(serviceName, _('QuarkDrive'),
			_('Expose your QuarkDrive files through WebDAV. The backend package is optional; you can deploy a compatible binary yourself.') + '<br />' +
			_('This backend serves HTTP only. Use a reverse proxy for HTTPS and do not expose an unauthenticated server to the Internet.'));

		s = m.section(form.NamedSection, 'config', 'quarkdrive_webdav');
		s.tab('basic', _('Basic settings'));
		s.tab('advanced', _('Advanced settings'));
		s.tab('log', _('Service log'));

		o = s.taboption('basic', form.Flag, 'enabled', _('Enable'));
		o.default = o.disabled;
		o.rmempty = false;

		o = s.taboption('basic', form.Value, 'binary_path', _('Binary path'));
		o.default = '/usr/bin/quarkdrive-webdav';
		o.rmempty = false;
		o.validate = absolutePath;
		o.description = _('Absolute path to an executable quarkdrive-webdav binary, for example /mnt/sda1/quarkdrive-webdav. It must match your router architecture.');

		o = s.taboption('basic', form.Value, 'quark_cookie', _('QuarkDrive Cookie'));
		o.password = true;
		o.description = _('Copy the complete Cookie header from a logged-in QuarkDrive browser session.');
		o.validate = function(sectionId, value) {
			const enabled = this.map.lookupOption('enabled', sectionId)[0].formvalue(sectionId);
			return enabled !== '1' || !!value || _('A QuarkDrive cookie is required when the service is enabled.');
		};

		o = s.taboption('basic', form.Value, 'host', _('Listen address'));
		o.datatype = 'ipaddr';
		o.placeholder = '0.0.0.0';

		o = s.taboption('basic', form.Value, 'port', _('Listen port'));
		o.datatype = 'and(port,min(1))';
		o.placeholder = '8080';

		o = s.taboption('basic', form.Value, 'root', _('Root directory'));
		o.placeholder = '/';
		o.validate = absolutePath;
		o.description = _('QuarkDrive directory to expose, not a local mount path.');

		const validateAuth = function(sectionId, value) {
			const other = this.map.lookupOption(this.option === 'auth_user' ? 'auth_password' : 'auth_user', sectionId)[0].formvalue(sectionId);
			return !!value === !!other || _('WebDAV username and password must be specified together.');
		};
		o = s.taboption('basic', form.Value, 'auth_user', _('WebDAV username'));
		o.description = _('Leave both username and password empty to disable authentication. This allows anyone who can reach the server to access your files.');
		o.validate = validateAuth;
		o = s.taboption('basic', form.Value, 'auth_password', _('WebDAV password'));
		o.password = true;
		o.validate = validateAuth;

		o = s.taboption('basic', form.Flag, 'read_only', _('Read-only mode'));
		o.description = _('Prevent uploads, changes and deletions through WebDAV.');

		[
			['read_buffer_size', _('Download buffer size (bytes)'), '10485760', 'and(uinteger,min(1))'],
			['cache_size', _('Directory cache size'), '1000', 'uinteger'],
			['cache_ttl', _('Directory cache TTL (seconds)'), '600', 'uinteger'],
			['refresh_cache_secs_interval', _('Cache refresh interval (seconds)'), '300', 'and(uinteger,min(1))'],
			['upload_wait_timeout', _('Upload completion timeout (seconds)'), '280', 'uinteger']
		].forEach(item => {
			o = s.taboption('advanced', form.Value, item[0], item[1]);
			o.placeholder = item[2];
			o.datatype = item[3];
			if (item[0] === 'upload_wait_timeout')
				o.description = _('Return success after this timeout while the upload continues in the background. Set to 0 to wait indefinitely.');
		});

		o = s.taboption('advanced', form.Value, 'strip_prefix', _('URL prefix to strip'));
		o.validate = absolutePath;
		o.description = _('Remove this prefix from incoming request paths when using a reverse proxy.');
		o = s.taboption('advanced', form.Flag, 'skip_upload_same_size', _('Skip same-size uploads'));
		o = s.taboption('advanced', form.Flag, 'redirect', _('Enable redirects when possible'));
		o = s.taboption('advanced', form.Flag, 'debug', _('Enable debug logging'));
		o.description = _('Debug logs may contain sensitive information. Disable this after troubleshooting.');

		o = s.taboption('log', form.DummyValue, '_log');
		o.renderWidget = () => logs;

		return m.render().then(map => {
			updateButtons();
			poll.add(() => Promise.all([
				refreshStatus(),
				fs.exec(initPath, ['log']).then(result => {
					if (result.code !== 0)
						throw new Error(result.stderr || _('Unable to read service logs.'));
					const atBottom = logs.scrollTop + logs.clientHeight >= logs.scrollHeight - 10;
					const text = (result.stdout || '').replace(/\x1b\[[0-9;]*m/g, '') || _('No service logs yet.');
					if (logs.value !== text) {
						logs.value = text;
						if (atBottom)
							logs.scrollTop = logs.scrollHeight;
					}
				}).catch(error => { logs.value = error.message; })
			]), 5);

			return refreshStatus().then(() => E('div', {}, [
				E('div', { 'class': 'cbi-section' }, [
					E('p', {}, [status]),
					E('p', {}, controls),
					E('p', {}, _('Save & Apply before starting or restarting. Start requires Enable to be checked.'))
				]),
				map
			]));
		});
	}
});
