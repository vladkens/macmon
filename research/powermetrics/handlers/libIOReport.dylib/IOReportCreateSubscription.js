function describeCfObject(value) {
  if (value.isNull() || !ObjC.available) {
    return null;
  }

  try {
    return new ObjC.Object(value).toString();
  } catch (error) {
    return `<description failed: ${error}>`;
  }
}

defineHandler({
  onEnter(log, args, state) {
    state.requestedChannels = args[1];
    state.subscribedChannelsOut = args[2];
    state.flags = args[3];
    state.options = args[4];
    state.requestedDescription = describeCfObject(args[1]);
  },

  onLeave(log, retval, state) {
    let subscribedChannels = ptr(0);
    if (!state.subscribedChannelsOut.isNull()) {
      try {
        subscribedChannels = state.subscribedChannelsOut.readPointer();
      } catch (error) {
        subscribedChannels = ptr(0);
      }
    }

    log(
      `IOReportCreateSubscription(requested=${state.requestedChannels}, ` +
        `subscribed=${subscribedChannels}, flags=${state.flags}, ` +
        `options=${state.options}) -> subscription=${retval}`,
    );
    log(
      `IOReportSubscriptionChannels(subscription=${retval}, ` +
        `requested=${JSON.stringify(state.requestedDescription)}, ` +
        `subscribed=${JSON.stringify(describeCfObject(subscribedChannels))})`,
    );
  },
});
