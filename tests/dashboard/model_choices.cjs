// Run with node --test tests/dashboard/model_choices.cjs
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');
const script = readFileSync('src/dashboard.html', 'utf8').match(/<script>([\s\S]*)<\/script>/)[1];
function setup(fetch) {
  const elements = {};
  const context = vm.createContext({ fetch, document: {
    addEventListener() {},
    createElement() { return {}; },
    getElementById(id) { return elements[id] ||= { value: '', textContent: '', replaceChildren(...rows) { this.rows = rows; } }; }
  }});
  vm.runInContext(script, context);
  const get = id => context.document.getElementById('cfg-default-' + id);
  get('harness').value = 'pi-native';
  get('provider').value = 'stale-provider';
  get('model').value = 'old-model';
  return { context, get, update: reason => context.updateModelChoices('cfg-default', reason) };
}
const rows = [{provider:'anthropic', model:'claude-test'}, {provider:'hub', model:'vendor/test'}, {provider:'openai', model:'gpt-test'}];
const success = async () => ({ok:true, json:async () => ({models:rows})});
test('engine and provider changes select matching models, native filters unsupported providers', async () => {
  const {get, update} = setup(success);
  await update('harness');
  assert.equal(get('provider').value, 'anthropic');
  assert.equal(get('model').value, 'claude-test');
  assert.equal(get('provider-list').rows.length, 2);
  get('provider').value = 'openai'; await update('provider');
  assert.equal(get('model').value, 'gpt-test');
  get('harness').value = 'pi-rpc'; await update('harness');
  assert.equal(get('provider-list').rows.length, 3);
  get('harness').value = 'codex'; await update('harness');
  assert.equal(get('provider').value, 'openai');
  assert.equal(get('model').value, 'gpt-6-astra');
});
test('hydration preserves custom config and failures allow manual entry', async () => {
  const {get, update} = setup(success);
  await update(); assert.equal(get('model').value, 'old-model');
  const failed = setup(async () => {throw Error('unavailable');});
  await failed.update('harness');
  assert.equal(failed.get('model').value, 'old-model');
  assert.match(failed.get('catalog-status').textContent, /unavailable/);
});
test('late catalog response cannot overwrite a newer harness selection', async () => {
  let resolve;
  const {get, update} = setup(() => new Promise(r => {resolve = r;}));
  const pending = update('harness');
  get('harness').value = 'claude-code'; await update('harness');
  resolve(await success()); await pending;
  assert.equal(get('model').value, 'claude-sonnet-4-6');
  assert.equal(get('model-list').rows.length, 0);
});
