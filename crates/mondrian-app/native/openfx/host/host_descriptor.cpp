// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause

#include <iostream>
#include <fstream>
#include <cstdio>
#include <cstring>

// ofx
#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxPixels.h"

// ofx host
#include "ofxhBinary.h"
#include "ofxhPropertySuite.h"
#include "ofxhClip.h"
#include "ofxhParam.h"
#include "ofxhMemory.h"
#include "ofxhImageEffect.h"
#include "ofxhPluginAPICache.h"
#include "ofxhPluginCache.h"
#include "ofxhHost.h"
#include "ofxhImageEffectAPI.h"

// Mondrian's filter host
#include "host_descriptor.h"
#include "effect_instance.h"
#include "image_clip.h"

namespace MondrianOpenFx
{
  Host::Host()
  {
    _properties.setIntProperty(kOfxPropAPIVersion, 1, 0);
    _properties.setIntProperty(kOfxPropAPIVersion, 4, 1);
    _properties.setStringProperty(kOfxPropName, "Mondrian");
    _properties.setStringProperty(kOfxPropLabel, "Mondrian");
    _properties.setIntProperty(kOfxPropVersion, 1, 0);
    _properties.setIntProperty(kOfxPropVersion, 0, 1);
    _properties.setStringProperty(kOfxPropVersionLabel, "1.0");
    _properties.setIntProperty(kOfxImageEffectHostPropIsBackground, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsOverlays, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsMultiResolution, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsTiles, false);
    _properties.setIntProperty(kOfxImageEffectPropTemporalClipAccess, false);
    _properties.setStringProperty(kOfxImageEffectPropSupportedComponents, kOfxImageComponentRGBA, 0);
    _properties.setStringProperty(kOfxImageEffectPropSupportedContexts, kOfxImageEffectContextFilter, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsMultipleClipDepths, 0);
    _properties.setIntProperty(kOfxImageEffectPropSupportsMultipleClipPARs, 0);
    _properties.setIntProperty(kOfxImageEffectPropSetableFrameRate, 0);
    _properties.setIntProperty(kOfxImageEffectPropSetableFielding, 0);
    _properties.setIntProperty(kOfxParamHostPropSupportsCustomInteract, 0 );
    _properties.setIntProperty(kOfxParamHostPropSupportsStringAnimation, 0 );
    _properties.setIntProperty(kOfxParamHostPropSupportsChoiceAnimation, 0 );
    _properties.setIntProperty(kOfxParamHostPropSupportsBooleanAnimation, 0 );
    _properties.setIntProperty(kOfxParamHostPropSupportsCustomAnimation, 0 );
    _properties.setIntProperty(kOfxParamHostPropMaxParameters, -1);
    _properties.setIntProperty(kOfxParamHostPropMaxPages, 0);
    _properties.setIntProperty(kOfxParamHostPropPageRowColumnCount, 0, 0 );
    _properties.setIntProperty(kOfxParamHostPropPageRowColumnCount, 0, 1 );
  }

  OFX::Host::ImageEffect::Instance* Host::newInstance(void* clientData,
                                                      OFX::Host::ImageEffect::ImageEffectPlugin* plugin,
                                                      OFX::Host::ImageEffect::Descriptor& desc,
                                                      const std::string& context)
  {
    return new FilterEffectInstance(plugin, desc, context);
  }

  /// Override this to create a descriptor, this makes the 'root' descriptor
  OFX::Host::ImageEffect::Descriptor *Host::makeDescriptor(OFX::Host::ImageEffect::ImageEffectPlugin* plugin)
  {
    OFX::Host::ImageEffect::Descriptor *desc = new OFX::Host::ImageEffect::Descriptor(plugin);
    return desc;
  }

  /// used to construct a context description, rootContext is the main context
  OFX::Host::ImageEffect::Descriptor *Host::makeDescriptor(const OFX::Host::ImageEffect::Descriptor &rootContext,
                                                           OFX::Host::ImageEffect::ImageEffectPlugin *plugin)
  {
    OFX::Host::ImageEffect::Descriptor *desc = new OFX::Host::ImageEffect::Descriptor(rootContext, plugin);
    return desc;
  }

  /// used to construct populate the cache
  OFX::Host::ImageEffect::Descriptor *Host::makeDescriptor(const std::string &bundlePath,
                                                           OFX::Host::ImageEffect::ImageEffectPlugin *plugin)
  {
    OFX::Host::ImageEffect::Descriptor *desc = new OFX::Host::ImageEffect::Descriptor(bundlePath, plugin);
    return desc;
  }

  /// message
  OfxStatus Host::vmessage(const char* type,
                           const char* id,
                           const char* format,
                           va_list args)
  {
    bool isQuestion = false;
    const char *prefix = "Message : ";
    if (std::strcmp(type, kOfxMessageLog) == 0) {
      prefix = "Log : ";
    }
    else if(std::strcmp(type, kOfxMessageFatal) == 0 ||
            std::strcmp(type, kOfxMessageError) == 0) {
      prefix = "Error : ";
    }
    else if(std::strcmp(type, kOfxMessageQuestion) == 0)  {
      prefix = "Question : ";
      isQuestion = true;
    }

    // The isolated render worker has no interactive UI.
    std::fputs(prefix, stdout);
    std::vprintf(format, args);
    std::printf("\n");

    if(isQuestion) {
      return kOfxStatReplyNo;
    }
    else {
      return kOfxStatOK;
    }
  }

  OfxStatus Host::setPersistentMessage(const char* type,
                                       const char* id,
                                       const char* format,
                                       va_list args)
  {
    return vmessage(type, id, format, args);
  }

  OfxStatus Host::clearPersistentMessage()
  {
    return kOfxStatOK;
  }
}
