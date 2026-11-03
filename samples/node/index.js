// Requires old lodash, minimist and axios releases on purpose.
const _ = require("lodash");
const minimist = require("minimist");
const axios = require("axios");

console.log(_.get({ a: { b: 1 } }, "a.b"), minimist(process.argv.slice(2)), typeof axios.get);
