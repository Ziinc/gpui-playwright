// The page facade scripts run against, and Playwright's value encoding.
// `__gpwQuery` and `__gpwExposed_*` are native functions the server adds.
(() => {
  const serialize = (value, seen) => {
    if (value === undefined) return { v: 'undefined' };
    if (value === null) return { v: 'null' };
    switch (typeof value) {
      case 'number':
        if (Number.isNaN(value)) return { v: 'NaN' };
        if (value === Infinity) return { v: 'Infinity' };
        if (value === -Infinity) return { v: '-Infinity' };
        if (Object.is(value, -0)) return { v: '-0' };
        return { n: value };
      case 'boolean':
        return { b: value };
      case 'string':
        return { s: value };
      case 'bigint':
        return { bi: value.toString() };
      case 'function':
      case 'symbol':
        return { v: 'undefined' };
    }
    if (value instanceof Date) return { d: value.toJSON() };
    if (value instanceof RegExp) return { r: { p: value.source, f: value.flags } };
    if (value instanceof Error) {
      return { e: { n: value.name, m: value.message, s: value.stack || '' } };
    }
    // Elements serialize as Playwright's do when returned by value.
    if (value && value.__gpwElement) return { o: [], id: seen.size + 1 };
    if (seen.has(value)) return { ref: seen.get(value) };
    const id = seen.size + 1;
    seen.set(value, id);
    if (Array.isArray(value)) return { a: value.map((item) => serialize(item, seen)), id };
    return {
      o: Object.keys(value).map((k) => ({ k, v: serialize(value[k], seen) })),
      id,
    };
  };

  const parse = (value, handles, refs) => {
    if (value === undefined || value === null) return undefined;
    if ('v' in value) {
      switch (value.v) {
        case 'null':
          return null;
        case 'NaN':
          return NaN;
        case 'Infinity':
          return Infinity;
        case '-Infinity':
          return -Infinity;
        case '-0':
          return -0;
        default:
          return undefined;
      }
    }
    if ('n' in value) return value.n;
    if ('s' in value) return value.s;
    if ('b' in value) return value.b;
    if ('bi' in value) return BigInt(value.bi);
    if ('d' in value) return new Date(value.d);
    if ('r' in value) return new RegExp(value.r.p, value.r.f);
    if ('h' in value) return handles[value.h];
    if ('ref' in value) return refs.get(value.ref);
    if ('a' in value) {
      const array = [];
      refs.set(value.id, array);
      for (const item of value.a) array.push(parse(item, handles, refs));
      return array;
    }
    if ('o' in value) {
      const object = {};
      refs.set(value.id, object);
      for (const { k, v } of value.o) object[k] = parse(v, handles, refs);
      return object;
    }
    return undefined;
  };

  const ATTRIBUTES = {
    'aria-label': (d) => d.label ?? null,
    id: (d) => d.testId ?? null,
    'data-testid': (d) => d.testId ?? null,
    role: (d) => d.role ?? null,
    value: (d) => d.value ?? null,
    disabled: (d) => (d.disabled ? '' : null),
    'aria-checked': (d) => (d.checked === null || d.checked === undefined ? null : String(d.checked)),
  };

  const element = (d) => {
    const rect = () => ({
      x: d.bounds.x,
      y: d.bounds.y,
      width: d.bounds.width,
      height: d.bounds.height,
      top: d.bounds.y,
      left: d.bounds.x,
      right: d.bounds.x + d.bounds.width,
      bottom: d.bounds.y + d.bounds.height,
    });
    const getAttribute = (name) => {
      const read = ATTRIBUTES[name.toLowerCase()];
      if (read) return read(d);
      return d.attributes[name] ?? null;
    };
    const query = (selector) => JSON.parse(__gpwQuery(selector, d.index ?? undefined)).map(element);
    return {
      __gpwElement: true,
      tagName: (d.role || 'div').toUpperCase(),
      nodeName: (d.role || 'div').toUpperCase(),
      nodeType: 1,
      role: d.role ?? null,
      id: d.testId ?? '',
      textContent: d.text,
      innerText: d.text,
      value: d.value ?? '',
      checked: d.checked === true,
      indeterminate: d.checked === 'mixed',
      disabled: !!d.disabled,
      ariaLabel: d.label ?? null,
      title: d.attributes.title ?? '',
      placeholder: d.attributes.placeholder ?? '',
      alt: d.attributes.alt ?? '',
      isConnected: true,
      getAttribute,
      hasAttribute: (name) => getAttribute(name) !== null,
      getBoundingClientRect: rect,
      querySelector: (selector) => query(selector)[0] ?? null,
      querySelectorAll: (selector) => query(selector),
      matches: (selector) =>
        JSON.parse(__gpwQuery(selector, undefined)).some((found) => found.index === d.index),
      focus: () => {},
      blur: () => {},
    };
  };

  globalThis.__gpwSetup = (setupJson) => {
    const setup = JSON.parse(setupJson);
    const body = element(setup.body);
    globalThis.window = globalThis;
    globalThis.self = globalThis;
    globalThis.innerWidth = setup.viewport[0];
    globalThis.innerHeight = setup.viewport[1];
    globalThis.location = { href: setup.url, toString: () => setup.url };
    globalThis.document = {
      title: setup.title,
      body,
      documentElement: body,
      readyState: 'complete',
      querySelector: (selector) => body.querySelector(selector),
      querySelectorAll: (selector) => body.querySelectorAll(selector),
    };
    for (const name of setup.exposed) {
      const native = globalThis[`__gpwExposed_${name}`];
      globalThis[name] = (...args) => JSON.parse(native(JSON.stringify(args)));
    }
  };

  globalThis.__gpwInvoke = (fn, isFunction, subjectJson, argJson, handlesJson) => {
    if (!isFunction) return fn();
    const handles = JSON.parse(handlesJson).map((h) =>
      'element' in h ? element(h.element) : parse(h.value, [], new Map()),
    );
    const arg = parse(JSON.parse(argJson)?.value, handles, new Map());
    const subject = JSON.parse(subjectJson);
    if (subject === null) return fn(arg);
    if ('one' in subject) return fn(element(subject.one), arg);
    if ('value' in subject) return fn(parse(subject.value, [], new Map()), arg);
    return fn(subject.all.map(element), arg);
  };

  globalThis.__gpwSerialize = (value) => JSON.stringify(serialize(value, new Map()));
})();
